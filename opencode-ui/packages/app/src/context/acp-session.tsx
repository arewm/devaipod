/**
 * ACP session context for the devaipod agent page.
 *
 * Manages a WebSocket connection to pod-api's /ws/events endpoint,
 * parses incoming ACP events, and provides reactive signals for
 * messages, tool calls, permission requests, and connection state.
 */
import {
  createContext,
  useContext,
  onCleanup,
  type ParentProps,
  batch,
  createEffect,
} from "solid-js"
import { createStore, produce } from "solid-js/store"
import { getAuthToken } from "@/utils/devaipod-api"
import type {
  AcpMessage,
  ToolCall,
  PermissionRequest,
  WsEnvelope,
  WsCommand,
  ContentBlock,
  ToolCallStatus,
  ToolCallContent,
  ToolCallDiff,
  SlashCommand,
  SessionModeState,
} from "@/types/acp"

// ---------------------------------------------------------------------------
// Store shape
// ---------------------------------------------------------------------------

export type ConnectionState = "connecting" | "connected" | "disconnected" | "error"

/** Per-session state (messages, tool calls, etc.) */
export interface SessionData {
  messages: AcpMessage[]
  toolCalls: Record<string, ToolCall>
  pendingPermissions: PermissionRequest[]
  prompting: boolean
}

/** Per-pane state */
export interface PaneState {
  id: string           // unique pane ID
  tabs: string[]       // session IDs loaded as tabs in this pane
  activeTab: string    // which tab is currently showing
  width: number        // width as percentage (0-100)
}

interface AcpSessionStore {
  connectionState: ConnectionState
  connectionError: string | undefined
  /** Per-session data keyed by session ID. */
  sessionData: Record<string, SessionData>
  /** Panes with their tabs */
  panes: PaneState[]
  /** Currently focused pane (for keyboard routing). */
  activePaneId: string | undefined
  /** Slash commands advertised by the agent. */
  availableCommands: SlashCommand[]
  /** Session mode state (current mode + available modes). */
  sessionMode: SessionModeState | null
  /** Session IDs hidden by the user (x on tab). */
  hiddenSessions: string[]
  /** Available sessions from the agent. */
  sessions: Array<{ id: string; title?: string; created?: string }>
}

interface AcpSessionActions {
  /** Send a text prompt to the agent in a specific session. */
  sendPrompt: (text: string, sessionId?: string) => void
  /** Respond to a permission request. */
  respondPermission: (requestId: number | string, optionId: string) => void
  /** Cancel the current prompt turn. */
  cancelPrompt: () => void
  /** Load a specific session by ID (adds as a tab to the active pane). */
  loadSession: (sessionId: string) => void
  /** Create a new session (adds tab to active pane). */
  newSession: () => void
  /** Set the active tab in a pane. */
  setActiveTab: (paneId: string, sessionId: string) => void
  /** Close a pane. */
  closePane: (paneId: string) => void
  /** Close a tab from a pane. */
  closeTab: (paneId: string, sessionId: string) => void
  /** Split the active pane (create a new empty pane). */
  splitPane: () => void
  /** Set the active pane. */
  setActivePane: (paneId: string) => void
  /** Move a tab from one pane to another. */
  moveTab: (sessionId: string, fromPaneId: string, toPaneId: string, insertBeforeSessionId?: string) => void
  /** Restore a hidden session (add it back as a tab in the active pane). */
  restoreSession: (sessionId: string) => void
  /** Get data for a specific session. */
  getSessionData: (sessionId: string) => SessionData
  /** Set pane width (percentage). */
  setPaneWidth: (paneId: string, width: number) => void
}

export type AcpSessionContext = AcpSessionStore & AcpSessionActions

const AcpCtx = createContext<AcpSessionContext>()

export function useAcpSession(): AcpSessionContext {
  const ctx = useContext(AcpCtx)
  if (!ctx) throw new Error("useAcpSession must be used inside AcpSessionProvider")
  return ctx
}

// ---------------------------------------------------------------------------
// Helper: extract text from a content block
// ---------------------------------------------------------------------------

function contentText(block: ContentBlock): string {
  if (block.type === "text") return block.text
  return ""
}

/** Factory for creating empty session data. */
function emptySessionData(): SessionData {
  return {
    messages: [],
    toolCalls: {},
    pendingPermissions: [],
    prompting: false,
  }
}

/** Sort sessions by created date. */
function sortSessionsByDate(
  sessions: Array<{ id: string; title?: string; created?: string }>,
  order: "asc" | "desc"
): Array<{ id: string; title?: string; created?: string }> {
  return [...sessions].sort((a, b) => {
    const ta = a.created ? new Date(a.created).getTime() : 0
    const tb = b.created ? new Date(b.created).getTime() : 0
    return order === "asc" ? ta - tb : tb - ta
  })
}

function toolContentText(items: Array<ToolCallContent | ToolCallDiff>): string {
  return items
    .map((item) => {
      if (item.type === "content") return contentText(item.content)
      if (item.type === "diff") return `diff ${item.path}`
      return ""
    })
    .filter(Boolean)
    .join("\n")
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------


interface SavedLayout {
  panes: Array<{
    id: string
    tabs: string[]
    activeTab: string
    width: number
  }>
  activePaneId: string
}

export function AcpSessionProvider(props: ParentProps<{ podName: string }>) {
  // Module-level counters moved inside provider scope so they reset on remount.
  let msgCounter = 0
  let paneCounter = 0

  // Load hidden sessions from localStorage
  const hiddenKey = `devaipod-hidden-sessions-${props.podName}`
  const loadHidden = (): string[] => {
    try {
      const raw = localStorage.getItem(hiddenKey)
      return raw ? JSON.parse(raw) : []
    } catch { return [] }
  }

  const [store, setStore] = createStore<AcpSessionStore>({
    connectionState: "connecting",
    connectionError: undefined,
    sessionData: {},
    panes: [],
    activePaneId: undefined,
    hiddenSessions: loadHidden(),
    availableCommands: [],
    sessionMode: null,
    sessions: [],
  })

  // Track which pane a hidden session came from (for restore)
  const hiddenFromPane: Record<string, string> = {}

  let ws: WebSocket | undefined
  let reconnectTimer: ReturnType<typeof setTimeout> | undefined
  const RECONNECT_DELAY_MS = 3000

  // Layout persistence
  const layoutKey = `devaipod-pane-layout-${props.podName}`
  const loadLayout = (): SavedLayout | null => {
    try {
      const raw = localStorage.getItem(layoutKey)
      return raw ? JSON.parse(raw) : null
    } catch { return null }
  }

  const saveLayout = () => {
    try {
      const layout: SavedLayout = {
        panes: store.panes.map((p) => ({
          id: p.id,
          tabs: p.tabs,
          activeTab: p.activeTab,
          width: p.width,
        })),
        activePaneId: store.activePaneId || "",
      }
      localStorage.setItem(layoutKey, JSON.stringify(layout))
    } catch {
      // Ignore save errors
    }
  }

  function buildWsUrl(): string {
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:"
    const token = getAuthToken()
    let url = `${proto}//${window.location.host}/api/devaipod/pods/${encodeURIComponent(props.podName)}/pod-api/ws/events`
    if (token) url += `?token=${encodeURIComponent(token)}`
    return url
  }

  function sendWs(cmd: WsCommand) {
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(cmd))
    }
  }

  /** Clear session data and request a replay from the server. */
  /** Clear session data and request a replay from the server.
   *  IMPORTANT: This must call sendWs(), NOT itself. A prior bug had
   *  this calling reloadSession() recursively (infinite recursion). */
  function reloadSession(sessionId: string) {
    setStore(
      produce((s) => {
        s.sessionData[sessionId] = emptySessionData()
      }),
    )
    sendWs({ type: "load_session", sessionId })
  }

  function connect() {
    if (ws) {
      ws.onclose = null
      ws.onerror = null
      ws.onmessage = null
      ws.close()
    }

    setStore("connectionState", "connecting")
    setStore("connectionError", undefined)

    const url = buildWsUrl()
    ws = new WebSocket(url)

    ws.onopen = () => {
      setStore("connectionState", "connected")
    }

    ws.onclose = () => {
      setStore("connectionState", "disconnected")
      scheduleReconnect()
    }

    ws.onerror = () => {
      setStore("connectionState", "error")
      setStore("connectionError", "WebSocket connection failed")
    }

    ws.onmessage = (event) => {
      try {
        const envelope: WsEnvelope = JSON.parse(event.data)
        handleEnvelope(envelope)
      } catch {
        // Ignore unparseable messages
      }
    }
  }

  function scheduleReconnect() {
    if (reconnectTimer) clearTimeout(reconnectTimer)
    reconnectTimer = setTimeout(connect, RECONNECT_DELAY_MS)
  }

  function handleEnvelope(envelope: WsEnvelope) {
    switch (envelope.type) {
      case "keepalive":
        // No-op, connection is alive
        break

      case "connection_status":
        if (envelope.status === "error") {
          setStore("connectionError", envelope.message)
        }
        break

      case "session_update":
        handleSessionUpdate(envelope.sessionId, envelope.update)
        break

      case "permission_request":
        handlePermissionRequest(envelope.request)
        break

      case "prompt_response": {
        // Clear prompting for the session identified in the response envelope.
        setStore(
          produce((s) => {
            if (s.sessionData[envelope.sessionId]) {
              s.sessionData[envelope.sessionId].prompting = false
            }
          }),
        )
        break
      }

      case "session_list": {
        // The sessions payload may be { sessions: [...] } (from OpenCode)
        // or a flat array. Handle both.
        const raw = envelope.sessions
        const sessionArray = Array.isArray(raw)
          ? raw
          : Array.isArray(raw?.sessions)
            ? raw.sessions
            : []
        const sessions = sessionArray.map((s: { sessionId: string; title?: string; created?: string; updatedAt?: string }) => ({
          id: s.sessionId,
          title: s.title,
          created: s.created || s.updatedAt,
        }))
        setStore("sessions", sessions)

        // If we don't have any panes yet, try to restore layout or create default
        if (sessions.length > 0 && store.panes.length === 0) {
          const hidden = new Set(store.hiddenSessions)
          const visible = sessions.filter((ss) => !hidden.has(ss.id))
          if (visible.length === 0) {
            // All sessions hidden — clear hidden list and show all
            // (stale hidden IDs from old sessions shouldn't block everything)
            setStore("hiddenSessions", [])
            localStorage.removeItem(hiddenKey)
            // Re-run with no hidden filter
            const allSorted = sortSessionsByDate(sessions, "asc")
            const allNewest = sortSessionsByDate(sessions, "desc")[0]
            const paneId = `pane-${++paneCounter}`
            setStore(
              produce((s) => {
                s.panes = [{
                  id: paneId,
                  tabs: allSorted.map((ss) => ss.id),
                  activeTab: allNewest.id,
                  width: 100,
                }]
                s.activePaneId = paneId
              }),
            )
            reloadSession(allNewest.id)
            break
          }

          const savedLayout = loadLayout()
          const sessionIds = new Set(sessions.map((s) => s.id))

          // Check if saved layout is valid (all session IDs exist)
          if (
            savedLayout &&
            savedLayout.panes.length > 0 &&
            savedLayout.panes.every((p) =>
              p.tabs.every((tabId) => sessionIds.has(tabId))
            )
          ) {
            // Restore saved layout
            setStore(
              produce((s) => {
                s.panes = savedLayout.panes.map((p) => ({
                  id: p.id,
                  tabs: p.tabs,
                  activeTab: p.activeTab,
                  width: p.width,
                }))
                s.activePaneId = savedLayout.activePaneId
                // Update paneCounter to prevent ID collisions
                for (const p of savedLayout.panes) {
                  const num = parseInt(p.id.replace(/^pane-/, ""), 10)
                  if (!isNaN(num) && num >= paneCounter) {
                    paneCounter = num
                  }
                }
              }),
            )
            // Load each pane's active session history
            for (const p of savedLayout.panes) {
              if (p.activeTab) {
                reloadSession(p.activeTab)
              }
            }
          } else {
            // Create default layout: one pane with all visible sessions
            const sorted = sortSessionsByDate(visible, "asc")
            const newest = sortSessionsByDate(visible, "desc")[0]
            const paneId = `pane-${++paneCounter}`
            setStore(
              produce((s) => {
                s.panes = [
                  {
                    id: paneId,
                    tabs: sorted.map((ss) => ss.id),
                    activeTab: newest.id,
                    width: 100,
                  },
                ]
                s.activePaneId = paneId
              }),
            )
            // Load the active session's history
            reloadSession(newest.id)
          }
        }
        break
      }

      case "session_created": {
        // Add the new session as a tab to the active pane
        setStore(
          produce((s) => {
            const activePane = s.panes.find((p) => p.id === s.activePaneId)
            if (activePane) {
              if (!activePane.tabs.includes(envelope.sessionId)) {
                activePane.tabs.push(envelope.sessionId)
              }
              activePane.activeTab = envelope.sessionId
            } else {
              // No active pane, create one
              const paneId = `pane-${++paneCounter}`
              s.panes.push({
                id: paneId,
                tabs: [envelope.sessionId],
                activeTab: envelope.sessionId,
                width: 100,
              })
              s.activePaneId = paneId
            }
          }),
        )
        // Refresh session list
        sendWs({ type: "list_sessions" })
        break
      }
    }
  }

  function handleSessionUpdate(sessionId: string, update: WsEnvelope extends { type: "session_update" } ? WsEnvelope["update"] : never) {
    // Initialize session data if it doesn't exist
    setStore(
      produce((s) => {
        if (!s.sessionData[sessionId]) {
          s.sessionData[sessionId] = emptySessionData()
        }
      }),
    )

    switch (update.sessionUpdate) {
      case "agent_message_chunk": {
        const text = contentText(update.content)
        if (!text) break
        appendOrUpdateMessage(sessionId, "assistant", text)
        break
      }

      case "user_message_chunk": {
        const text = contentText(update.content)
        if (!text) break
        appendOrUpdateMessage(sessionId, "user", text)
        break
      }

      case "thought_chunk": {
        const text = contentText(update.content)
        if (!text) break
        appendOrUpdateMessage(sessionId, "thought", text)
        break
      }

      case "tool_call": {
        setStore(
          produce((s) => {
            if (!s.sessionData[sessionId]) return
            s.sessionData[sessionId].toolCalls[update.toolCallId] = {
              toolCallId: update.toolCallId,
              title: update.title,
              kind: update.kind,
              status: update.status ?? "pending",
              content: update.content,
              locations: update.locations,
              rawInput: update.rawInput,
            }
          }),
        )
        break
      }

      case "tool_call_update": {
        setStore(
          produce((s) => {
            if (!s.sessionData[sessionId]) return
            const existing = s.sessionData[sessionId].toolCalls[update.toolCallId]
            if (existing) {
              if (update.status) existing.status = update.status as ToolCallStatus
              if (update.title) existing.title = update.title
              if (update.content) existing.content = update.content as Array<ToolCallContent | ToolCallDiff>
              if (update.locations) existing.locations = update.locations
              if (update.rawOutput) existing.rawOutput = update.rawOutput
            }
          }),
        )
        break
      }

      case "plan":
        // Plans are informational; could render them but keeping it simple
        break

      case "available_commands_update":
        setStore("availableCommands", update.availableCommands)
        break

      case "current_mode_update":
        setStore(
          produce((s) => {
            if (s.sessionMode) {
              s.sessionMode.currentModeId = update.modeId
            } else {
              s.sessionMode = {
                currentModeId: update.modeId,
                availableModes: [],
              }
            }
          }),
        )
        break
    }
  }

  /**
   * Append text to the last message if it has the same role (streaming),
   * or create a new message.
   */
  function appendOrUpdateMessage(sessionId: string, role: AcpMessage["role"], text: string) {
    setStore(
      produce((s) => {
        if (!s.sessionData[sessionId]) {
          s.sessionData[sessionId] = emptySessionData()
        }
        const messages = s.sessionData[sessionId].messages
        const last = messages[messages.length - 1]
        if (last && last.role === role) {
          // Streaming: append to existing message
          last.text += text
        } else {
          messages.push({
            id: `msg-${++msgCounter}`,
            role,
            text,
            timestamp: Date.now(),
          })
        }
      }),
    )
  }

  function handlePermissionRequest(request: PermissionRequest) {
    // Permission requests are handled by the backend's auto_approve AtomicBool.
    // If they reach the frontend, they need manual approval.
    // Show in UI for manual approval - route to active pane's active tab
    const activePane = store.panes.find((p) => p.id === store.activePaneId)
    if (!activePane) return
    const sessionId = activePane.activeTab

    setStore(
      produce((s) => {
        if (!s.sessionData[sessionId]) {
          s.sessionData[sessionId] = emptySessionData()
        }
        s.sessionData[sessionId].pendingPermissions.push(request)
      }),
    )
  }

  // -- Public actions -------------------------------------------------------

  function sendPrompt(text: string, sessionId?: string) {
    if (!text.trim()) return

    let sid = sessionId
    if (!sid) {
      // Default to active pane's active tab
      const activePane = store.panes.find((p) => p.id === store.activePaneId)
      if (!activePane) return
      sid = activePane.activeTab
    }

    // Add user message to local display
    setStore(
      produce((s) => {
        if (!s.sessionData[sid!]) {
          s.sessionData[sid!] = emptySessionData()
        }
        s.sessionData[sid!].messages.push({
          id: `msg-${++msgCounter}`,
          role: "user",
          text,
          timestamp: Date.now(),
        })
        s.sessionData[sid!].prompting = true
      }),
    )

    sendWs({
      type: "send_prompt",
      sessionId: sid,
      prompt: [{ type: "text", text }],
    })
  }

  function respondPermission(requestId: number | string, optionId: string) {
    sendWs({
      type: "permission_response",
      requestId,
      optionId,
    })

    // Remove from pending (check all sessions)
    setStore(
      produce((s) => {
        for (const sessionId in s.sessionData) {
          s.sessionData[sessionId].pendingPermissions = s.sessionData[sessionId].pendingPermissions.filter(
            (p) => p.requestId !== requestId,
          )
        }
      }),
    )
  }

  function cancelPrompt() {
    const activePane = store.panes.find((p) => p.id === store.activePaneId)
    if (activePane) {
      sendWs({
        type: "cancel_prompt",
        sessionId: activePane.activeTab,
      })
      setStore(
        produce((s) => {
          if (s.sessionData[activePane.activeTab]) {
            s.sessionData[activePane.activeTab].prompting = false
          }
        }),
      )
    }
  }


  // Save layout whenever panes change
  createEffect(() => {
    // Track panes to trigger effect
    const _ = store.panes
    if (store.panes.length > 0) {
      saveLayout()
    }
  })

  // Connect on mount
  connect()

  onCleanup(() => {
    if (reconnectTimer) clearTimeout(reconnectTimer)
    if (ws) {
      ws.onclose = null
      ws.close()
    }
  })

  function loadSession(sessionId: string) {
    setStore(
      produce((s) => {
        const activePane = s.panes.find((p) => p.id === s.activePaneId)
        if (activePane) {
          // Add to active pane's tabs if not already there
          if (!activePane.tabs.includes(sessionId)) {
            activePane.tabs.push(sessionId)
          }
          activePane.activeTab = sessionId
        } else {
          // No active pane, create one
          const paneId = `pane-${++paneCounter}`
          s.panes.push({
            id: paneId,
            tabs: [sessionId],
            activeTab: sessionId,
            width: 100,
          })
          s.activePaneId = paneId
        }
      }),
    )
    // reloadSession clears sessionData, so don't clear it here (double-clear)
    reloadSession(sessionId)
  }

  function newSession() {
    sendWs({ type: "new_session" })
    // The session_created event will handle adding to panes
  }

  function setActiveTab(paneId: string, sessionId: string) {
    setStore(
      produce((s) => {
        const pane = s.panes.find((p) => p.id === paneId)
        if (pane && pane.tabs.includes(sessionId)) {
          pane.activeTab = sessionId
        }
      }),
    )
    // Load session history if we don't have data for it yet
    if (!store.sessionData[sessionId] || store.sessionData[sessionId].messages.length === 0) {
      reloadSession(sessionId)
    }
  }

  function closePane(paneId: string) {
    setStore(
      produce((s) => {
        const paneIdx = s.panes.findIndex((p) => p.id === paneId)
        if (paneIdx >= 0) {
          s.panes.splice(paneIdx, 1)
          // If closing the active pane, switch to next available
          if (s.activePaneId === paneId && s.panes.length > 0) {
            s.activePaneId = s.panes[0].id
          }
          // Redistribute widths evenly
          if (s.panes.length > 0) {
            const newWidth = 100 / s.panes.length
            for (const pane of s.panes) {
              pane.width = newWidth
            }
          }
        }
      }),
    )
  }

  function closeTab(paneId: string, sessionId: string) {
    // Track source pane for restore
    hiddenFromPane[sessionId] = paneId

    // Mark as hidden and persist
    setStore(
      produce((s) => {
        if (!s.hiddenSessions.includes(sessionId)) {
          s.hiddenSessions.push(sessionId)
        }
      }),
    )
    localStorage.setItem(hiddenKey, JSON.stringify(store.hiddenSessions))

    setStore(
      produce((s) => {
        const pane = s.panes.find((p) => p.id === paneId)
        if (!pane) return

        const tabIdx = pane.tabs.indexOf(sessionId)
        if (tabIdx < 0) return

        pane.tabs.splice(tabIdx, 1)

        // If no tabs left, remove the pane and redistribute widths
        if (pane.tabs.length === 0) {
          const paneIdx = s.panes.findIndex((p) => p.id === paneId)
          s.panes.splice(paneIdx, 1)
          if (s.activePaneId === paneId && s.panes.length > 0) {
            s.activePaneId = s.panes[0].id
          }
          if (s.panes.length > 0) {
            const newWidth = 100 / s.panes.length
            for (const p of s.panes) {
              p.width = newWidth
            }
          }
        } else {
          if (pane.activeTab === sessionId) {
            pane.activeTab = pane.tabs[Math.max(0, tabIdx - 1)]
          }
        }
      }),
    )
  }

  function splitPane() {
    setStore(
      produce((s) => {
        // Distribute width evenly across all panes
        const newPaneCount = s.panes.length + 1
        const newWidth = 100 / newPaneCount
        for (const pane of s.panes) {
          pane.width = newWidth
        }
        const paneId = `pane-${++paneCounter}`
        s.panes.push({
          id: paneId,
          tabs: [],
          activeTab: "",
          width: newWidth,
        })
        s.activePaneId = paneId
      }),
    )
  }

  function setActivePane(paneId: string) {
    setStore("activePaneId", paneId)
  }

  function moveTab(sessionId: string, fromPaneId: string, toPaneId: string, insertBeforeSessionId?: string) {
    // Same pane: reorder
    if (fromPaneId === toPaneId) {
      if (!insertBeforeSessionId) return
      setStore(
        produce((s) => {
          const pane = s.panes.find((p) => p.id === fromPaneId)
          if (!pane) return
          const fromIdx = pane.tabs.indexOf(sessionId)
          if (fromIdx < 0) return
          pane.tabs.splice(fromIdx, 1)
          // "__end__" means append to the end
          const toIdx = insertBeforeSessionId === "__end__"
            ? pane.tabs.length
            : pane.tabs.indexOf(insertBeforeSessionId)
          pane.tabs.splice(toIdx >= 0 ? toIdx : pane.tabs.length, 0, sessionId)
        }),
      )
      return
    }
    setStore(
      produce((s) => {
        const from = s.panes.find((p) => p.id === fromPaneId)
        const to = s.panes.find((p) => p.id === toPaneId)
        if (!from || !to) return

        // Remove from source pane
        from.tabs = from.tabs.filter((t) => t !== sessionId)
        if (from.activeTab === sessionId) {
          from.activeTab = from.tabs[0] || ""
        }

        // Add to target pane
        if (!to.tabs.includes(sessionId)) {
          to.tabs.push(sessionId)
        }
        to.activeTab = sessionId

        // Remove empty source pane and redistribute widths
        if (from.tabs.length === 0) {
          s.panes = s.panes.filter((p) => p.id !== fromPaneId)
          if (s.activePaneId === fromPaneId) {
            s.activePaneId = toPaneId
          }
          if (s.panes.length > 0) {
            const newWidth = 100 / s.panes.length
            for (const p of s.panes) {
              p.width = newWidth
            }
          }
        }
      }),
    )
  }

  function restoreSession(sessionId: string) {
    // Remove from hidden list and persist
    setStore(
      produce((s) => {
        s.hiddenSessions = s.hiddenSessions.filter((id) => id !== sessionId)
      }),
    )
    localStorage.setItem(hiddenKey, JSON.stringify(store.hiddenSessions))

    // Restore to the pane it was hidden from, or first pane, or create new
    const sourcePaneId = hiddenFromPane[sessionId]
    delete hiddenFromPane[sessionId]

    setStore(
      produce((s) => {
        // Try source pane first, then first pane
        const targetPane = s.panes.find((p) => p.id === sourcePaneId)
          || s.panes[0]
        if (targetPane) {
          if (!targetPane.tabs.includes(sessionId)) {
            targetPane.tabs.push(sessionId)
          }
          targetPane.activeTab = sessionId
        } else {
          const paneId = `pane-${++paneCounter}`
          s.panes.push({
            id: paneId,
            tabs: [sessionId],
            activeTab: sessionId,
            width: 100,
          })
          s.activePaneId = paneId
        }
      }),
    )
    // Load the session's history
    reloadSession(sessionId)
  }

  function getSessionData(sessionId: string): SessionData {
    return store.sessionData[sessionId] || emptySessionData()
  }

  function setPaneWidth(paneId: string, width: number) {
    setStore(
      produce((s) => {
        const pane = s.panes.find((p) => p.id === paneId)
        if (pane) {
          pane.width = width
        }
      }),
    )
  }

  const value: AcpSessionContext = {
    get connectionState() { return store.connectionState },
    get connectionError() { return store.connectionError },
    get sessionData() { return store.sessionData },
    get availableCommands() { return store.availableCommands },
    get sessionMode() { return store.sessionMode },
    get sessions() { return store.sessions },
    get panes() { return store.panes },
    get activePaneId() { return store.activePaneId },
    get hiddenSessions() { return store.hiddenSessions },
    // Backwards compat: getters that point to active pane's active tab's session data
    get messages() {
      const activePane = store.panes.find((p) => p.id === store.activePaneId)
      const sid = activePane?.activeTab
      return sid ? (store.sessionData[sid]?.messages ?? []) : []
    },
    get toolCalls() {
      const activePane = store.panes.find((p) => p.id === store.activePaneId)
      const sid = activePane?.activeTab
      return sid ? (store.sessionData[sid]?.toolCalls ?? {}) : {}
    },
    get pendingPermissions() {
      const activePane = store.panes.find((p) => p.id === store.activePaneId)
      const sid = activePane?.activeTab
      return sid ? (store.sessionData[sid]?.pendingPermissions ?? []) : []
    },
    get prompting() {
      const activePane = store.panes.find((p) => p.id === store.activePaneId)
      const sid = activePane?.activeTab
      return sid ? (store.sessionData[sid]?.prompting ?? false) : false
    },
    get sessionId() {
      const activePane = store.panes.find((p) => p.id === store.activePaneId)
      return activePane?.activeTab
    },
    sendPrompt,
    respondPermission,
    cancelPrompt,
    loadSession,
    newSession,
    setActiveTab,
    closePane,
    closeTab,
    splitPane,
    setActivePane,
    moveTab,
    restoreSession,
    getSessionData,
    setPaneWidth,
  }

  return <AcpCtx.Provider value={value}>{props.children}</AcpCtx.Provider>
}
