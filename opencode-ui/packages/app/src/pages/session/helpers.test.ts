import { describe, expect, test } from "bun:test"
import {
  combineCommandSections,
  createOpenReviewFile,
  focusTerminalById,
  getTabReorderIndex,
  isStaleSubmittedPrompt,
} from "./helpers"

describe("createOpenReviewFile", () => {
  test("opens and loads selected review file", () => {
    const calls: string[] = []
    const openReviewFile = createOpenReviewFile({
      showAllFiles: () => calls.push("show"),
      tabForPath: (path) => {
        calls.push(`tab:${path}`)
        return `file://${path}`
      },
      openTab: (tab) => calls.push(`open:${tab}`),
      loadFile: (path) => calls.push(`load:${path}`),
    })

    openReviewFile("src/a.ts")

    expect(calls).toEqual(["show", "tab:src/a.ts", "open:file://src/a.ts", "load:src/a.ts"])
  })
})

describe("focusTerminalById", () => {
  test("focuses textarea when present", () => {
    document.body.innerHTML = `<div id="terminal-wrapper-one"><div data-component="terminal"><textarea></textarea></div></div>`

    const focused = focusTerminalById("one")

    expect(focused).toBe(true)
    expect(document.activeElement?.tagName).toBe("TEXTAREA")
  })

  test("falls back to terminal element focus", () => {
    document.body.innerHTML = `<div id="terminal-wrapper-two"><div data-component="terminal" tabindex="0"></div></div>`
    const terminal = document.querySelector('[data-component="terminal"]') as HTMLElement
    let pointerDown = false
    terminal.addEventListener("pointerdown", () => {
      pointerDown = true
    })

    const focused = focusTerminalById("two")

    expect(focused).toBe(true)
    expect(document.activeElement).toBe(terminal)
    expect(pointerDown).toBe(true)
  })
})

describe("combineCommandSections", () => {
  test("keeps section order stable", () => {
    const result = combineCommandSections([
      [{ id: "a", title: "A" }],
      [
        { id: "b", title: "B" },
        { id: "c", title: "C" },
      ],
    ])

    expect(result.map((item) => item.id)).toEqual(["a", "b", "c"])
  })
})

describe("getTabReorderIndex", () => {
  test("returns target index for valid drag reorder", () => {
    expect(getTabReorderIndex(["a", "b", "c"], "a", "c")).toBe(2)
  })

  test("returns undefined for unknown droppable id", () => {
    expect(getTabReorderIndex(["a", "b", "c"], "a", "missing")).toBeUndefined()
  })
})

describe("isStaleSubmittedPrompt", () => {
  const userMsg = (id: string) => ({ role: "user", id })
  const assistantMsg = (id: string) => ({ role: "assistant", id })
  const textPart = (text: string) => ({ type: "text", text })

  test("detects prompt matching last user message", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "fix the bug" }],
        messages: [userMsg("m1"), assistantMsg("m2")],
        parts: { m1: [textPart("fix the bug")] },
      }),
    ).toBe(true)
  })

  test("matches after trimming whitespace", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "  fix the bug  " }],
        messages: [userMsg("m1")],
        parts: { m1: [textPart("fix the bug")] },
      }),
    ).toBe(true)
  })

  test("returns false for empty prompt", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "" }],
        messages: [userMsg("m1")],
        parts: { m1: [textPart("fix the bug")] },
      }),
    ).toBe(false)
  })

  test("returns false when prompt differs from last message", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "something new" }],
        messages: [userMsg("m1")],
        parts: { m1: [textPart("fix the bug")] },
      }),
    ).toBe(false)
  })

  test("returns false with no user messages", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "fix the bug" }],
        messages: [assistantMsg("m1")],
        parts: {},
      }),
    ).toBe(false)
  })

  test("returns false when message parts are not loaded yet", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "fix the bug" }],
        messages: [userMsg("m1")],
        parts: {},
      }),
    ).toBe(false)
  })

  test("ignores synthetic and ignored parts", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "fix the bug" }],
        messages: [userMsg("m1")],
        parts: {
          m1: [
            { type: "text", text: "system prompt", synthetic: true },
            { type: "text", text: "fix the bug" },
          ],
        },
      }),
    ).toBe(true)

    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "fix the bug" }],
        messages: [userMsg("m1")],
        parts: {
          m1: [{ type: "text", text: "fix the bug", synthetic: true }],
        },
      }),
    ).toBe(false)
  })

  test("uses last user message, not first", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "second message" }],
        messages: [userMsg("m1"), assistantMsg("m2"), userMsg("m3")],
        parts: {
          m1: [textPart("first message")],
          m3: [textPart("second message")],
        },
      }),
    ).toBe(true)

    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "first message" }],
        messages: [userMsg("m1"), assistantMsg("m2"), userMsg("m3")],
        parts: {
          m1: [textPart("first message")],
          m3: [textPart("second message")],
        },
      }),
    ).toBe(false)
  })

  test("handles multi-part prompt text", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [
          { type: "text", content: "fix " },
          { type: "file", content: "@src/main.ts" },
          { type: "text", content: " please" },
        ],
        messages: [userMsg("m1")],
        parts: { m1: [textPart("fix @src/main.ts please")] },
      }),
    ).toBe(true)
  })

  test("returns false for whitespace-only prompt", () => {
    expect(
      isStaleSubmittedPrompt({
        promptParts: [{ type: "text", content: "   " }],
        messages: [userMsg("m1")],
        parts: { m1: [textPart("fix the bug")] },
      }),
    ).toBe(false)
  })
})
