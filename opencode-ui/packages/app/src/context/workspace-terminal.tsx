/**
 * Workspace terminal context for devaipod.
 *
 * Manages PTY sessions in the workspace container via the devaipod control
 * plane PTY API (`/api/devaipod/pods/{name}/pty/*`).  The API shape mirrors
 * opencode's SDK PTY endpoints so the `Terminal` component can be reused.
 */

import { batch, createMemo, createSignal } from "solid-js"
import { createStore, produce } from "solid-js/store"
import type { LocalPTY } from "@/context/terminal"

// ---------------------------------------------------------------------------
// API types (match web_terminal.rs PtyInfo / PtyCreateInput)
// ---------------------------------------------------------------------------

interface PtyInfo {
  id: string
  title: string
  command: string
  args: string[]
  cwd: string
  status: string
  pid: number | null
}

// ---------------------------------------------------------------------------
// REST helpers
// ---------------------------------------------------------------------------

// The SPA runs on the pod-api sidecar which serves PTY endpoints at /pty/*.
// No pod name or control plane routing needed — it's the local origin.
function apiBase(): string {
  return "/pty"
}

async function createPty(title: string): Promise<PtyInfo> {
  const base = apiBase()
  const res = await fetch(base, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ title, container: "workspace" }),
  })
  if (!res.ok) throw new Error(`Create PTY failed: ${res.status}`)
  return res.json()
}

async function deletePty(id: string): Promise<void> {
  const base = apiBase()
  const res = await fetch(`${base}/${id}`, { method: "DELETE" })
  if (!res.ok && res.status !== 404) {
    throw new Error(`Delete PTY failed: ${res.status}`)
  }
}

export async function resizeWorkspacePty(id: string, cols: number, rows: number): Promise<void> {
  const base = apiBase()
  const res = await fetch(`${base}/${id}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ size: { rows, cols } }),
  })
  if (!res.ok) throw new Error(`Resize PTY failed: ${res.status}`)
}

export function buildWorkspaceWsUrl(ptyId: string): URL {
  const base = apiBase()
  return new URL(`${base}/${ptyId}/connect`, window.location.origin)
}

// ---------------------------------------------------------------------------
// Workspace terminal session manager
// ---------------------------------------------------------------------------

export function createWorkspaceTerminals() {
  const [store, setStore] = createStore<{
    active?: string
    all: LocalPTY[]
  }>({ all: [] })

  const [ready, setReady] = createSignal(true)

  const pickNextNumber = () => {
    const used = new Set(store.all.map((p) => p.titleNumber).filter((n) => n > 0))
    for (let i = 1; ; i++) {
      if (!used.has(i)) return i
    }
  }

  return {
    ready,
    all: createMemo(() => store.all),
    active: createMemo(() => store.active),

    new() {
      const nextNumber = pickNextNumber()
      createPty(`Workspace ${nextNumber}`)
        .then((info) => {
          const entry: LocalPTY = {
            id: info.id,
            title: info.title,
            titleNumber: nextNumber,
          }
          setStore("all", store.all.length, entry)
          setStore("active", info.id)
        })
        .catch((err) => console.error("Failed to create workspace terminal", err))
    },

    update(pty: Partial<LocalPTY> & { id: string }) {
      const index = store.all.findIndex((x) => x.id === pty.id)
      if (index >= 0) {
        setStore("all", index, (item) => ({ ...item, ...pty }))
      }
    },

    open(id: string) {
      setStore("active", id)
    },

    async close(id: string) {
      const index = store.all.findIndex((f) => f.id === id)
      if (index !== -1) {
        batch(() => {
          if (store.active === id) {
            const next = index > 0 ? store.all[index - 1]?.id : store.all[1]?.id
            setStore("active", next)
          }
          setStore(
            "all",
            produce((all) => {
              all.splice(index, 1)
            }),
          )
        })
      }
      await deletePty(id).catch((err) => console.error("Failed to close workspace terminal", err))
    },

    move(id: string, to: number) {
      const index = store.all.findIndex((f) => f.id === id)
      if (index === -1) return
      setStore(
        "all",
        produce((all) => {
          all.splice(to, 0, all.splice(index, 1)[0])
        }),
      )
    },

    next() {
      const index = store.all.findIndex((x) => x.id === store.active)
      if (index === -1) return
      setStore("active", store.all[(index + 1) % store.all.length]?.id)
    },

    previous() {
      const index = store.all.findIndex((x) => x.id === store.active)
      if (index === -1) return
      setStore("active", store.all[index === 0 ? store.all.length - 1 : index - 1]?.id)
    },
  }
}

export type WorkspaceTerminals = ReturnType<typeof createWorkspaceTerminals>
