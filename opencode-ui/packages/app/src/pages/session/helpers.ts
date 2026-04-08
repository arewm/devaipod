import type { CommandOption } from "@/context/command"
import { batch } from "solid-js"

export const focusTerminalById = (id: string) => {
  const wrapper = document.getElementById(`terminal-wrapper-${id}`)
  const terminal = wrapper?.querySelector('[data-component="terminal"]')
  if (!(terminal instanceof HTMLElement)) return false

  const textarea = terminal.querySelector("textarea")
  if (textarea instanceof HTMLTextAreaElement) {
    textarea.focus()
    return true
  }

  terminal.focus()
  terminal.dispatchEvent(
    typeof PointerEvent === "function"
      ? new PointerEvent("pointerdown", { bubbles: true, cancelable: true })
      : new MouseEvent("pointerdown", { bubbles: true, cancelable: true }),
  )
  return true
}

export const createOpenReviewFile = (input: {
  showAllFiles: () => void
  tabForPath: (path: string) => string
  openTab: (tab: string) => void
  loadFile: (path: string) => void
}) => {
  return (path: string) => {
    batch(() => {
      input.showAllFiles()
      input.openTab(input.tabForPath(path))
      input.loadFile(path)
    })
  }
}

export const combineCommandSections = (sections: readonly (readonly CommandOption[])[]) => {
  return sections.flatMap((section) => section)
}

export const getTabReorderIndex = (tabs: readonly string[], from: string, to: string) => {
  const fromIndex = tabs.indexOf(from)
  const toIndex = tabs.indexOf(to)
  if (fromIndex === -1 || toIndex === -1 || fromIndex === toIndex) return undefined
  return toIndex
}

/**
 * Detect whether the current prompt input is a stale copy of an
 * already-submitted message.
 *
 * In devaipod's iframe architecture, switching pods destroys and
 * recreates the iframe hosting the OpenCode web UI.  If the user
 * navigates away while the agent is still processing, the per-session
 * prompt state in localStorage may not reflect the `clearInput()` that
 * ran on submit.  When the iframe is recreated the stale prompt is
 * restored.
 *
 * This function returns `true` when the prompt text matches the last
 * user message, indicating it should be cleared.
 */
export function isStaleSubmittedPrompt(input: {
  promptParts: readonly { type: string; content?: string }[]
  messages: readonly { role: string; id: string }[]
  parts: Record<string, readonly { type: string; text?: string; synthetic?: boolean; ignored?: boolean }[] | undefined>
}): boolean {
  const text = input.promptParts
    .map((p) => ("content" in p && p.content ? p.content : ""))
    .join("")
    .trim()
  if (!text) return false

  const lastUser = findLastUserMessage(input.messages)
  if (!lastUser) return false

  const messageParts = input.parts[lastUser.id]
  if (!messageParts) return false

  const textPart = messageParts.find(
    (p) => p.type === "text" && !p.synthetic && !p.ignored,
  )
  if (!textPart || !textPart.text) return false

  return textPart.text.trim() === text
}

function findLastUserMessage(messages: readonly { role: string; id: string }[]) {
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === "user") return messages[i]
  }
  return undefined
}
