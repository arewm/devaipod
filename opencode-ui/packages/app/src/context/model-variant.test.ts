import { describe, expect, test } from "bun:test"
import { cycleModelVariant, fuzzyMatchModelID, getConfiguredAgentVariant, resolveModelVariant } from "./model-variant"

describe("model variant", () => {
  test("resolves configured agent variant when model matches", () => {
    const value = getConfiguredAgentVariant({
      agent: {
        model: { providerID: "openai", modelID: "gpt-5.2" },
        variant: "xhigh",
      },
      model: {
        providerID: "openai",
        modelID: "gpt-5.2",
        variants: { low: {}, high: {}, xhigh: {} },
      },
    })

    expect(value).toBe("xhigh")
  })

  test("ignores configured variant when model does not match", () => {
    const value = getConfiguredAgentVariant({
      agent: {
        model: { providerID: "openai", modelID: "gpt-5.2" },
        variant: "xhigh",
      },
      model: {
        providerID: "anthropic",
        modelID: "claude-sonnet-4",
        variants: { low: {}, high: {}, xhigh: {} },
      },
    })

    expect(value).toBeUndefined()
  })

  test("prefers selected variant over configured variant", () => {
    const value = resolveModelVariant({
      variants: ["low", "high", "xhigh"],
      selected: "high",
      configured: "xhigh",
    })

    expect(value).toBe("high")
  })

  test("cycles from configured variant to next", () => {
    const value = cycleModelVariant({
      variants: ["low", "high", "xhigh"],
      selected: undefined,
      configured: "high",
    })

    expect(value).toBe("xhigh")
  })

  test("wraps from configured last variant to first", () => {
    const value = cycleModelVariant({
      variants: ["low", "high", "xhigh"],
      selected: undefined,
      configured: "xhigh",
    })

    expect(value).toBe("low")
  })
})

describe("fuzzyMatchModelID", () => {
  const available = ["claude-sonnet-4@20250514", "claude-sonnet-4-6@default", "gpt-4o"]

  test("returns exact match when present", () => {
    expect(fuzzyMatchModelID("gpt-4o", available)).toBe("gpt-4o")
    expect(fuzzyMatchModelID("claude-sonnet-4@20250514", available)).toBe("claude-sonnet-4@20250514")
  })

  test("matches @default to dated variant by base name", () => {
    expect(fuzzyMatchModelID("claude-sonnet-4@default", available)).toBe("claude-sonnet-4@20250514")
  })

  test("does not match different base names", () => {
    expect(fuzzyMatchModelID("claude-opus-4@default", available)).toBeUndefined()
  })

  test("returns undefined for model with no @ suffix and no match", () => {
    expect(fuzzyMatchModelID("nonexistent-model", available)).toBeUndefined()
  })

  test("prefers exact base name over suffixed variant", () => {
    const withBare = ["claude-sonnet-4", "claude-sonnet-4@20250514"]
    expect(fuzzyMatchModelID("claude-sonnet-4@default", withBare)).toBe("claude-sonnet-4")
  })

  test("returns undefined for empty available list", () => {
    expect(fuzzyMatchModelID("claude-sonnet-4@default", [])).toBeUndefined()
  })

  test("handles @ at start of model ID", () => {
    // @ at position 0 means atIndex is 0, which is <= 0, so no fuzzy match
    expect(fuzzyMatchModelID("@default", available)).toBeUndefined()
  })
})
