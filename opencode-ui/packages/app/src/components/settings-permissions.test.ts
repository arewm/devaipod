import { describe, expect, test } from "bun:test"
import {
  ALL_TOOL_IDS,
  buildRestorePermissionMap,
  buildYoloPermissionMap,
  getAction,
  getRuleDefault,
  isYoloPermission,
  toMap,
} from "./settings-permissions"

describe("getAction", () => {
  test("returns the action for valid permission strings", () => {
    expect(getAction("allow")).toBe("allow")
    expect(getAction("ask")).toBe("ask")
    expect(getAction("deny")).toBe("deny")
  })

  test("returns undefined for invalid values", () => {
    expect(getAction(undefined)).toBeUndefined()
    expect(getAction(null)).toBeUndefined()
    expect(getAction("")).toBeUndefined()
    expect(getAction("yolo")).toBeUndefined()
    expect(getAction(42)).toBeUndefined()
    expect(getAction({})).toBeUndefined()
    expect(getAction([])).toBeUndefined()
  })
})

describe("getRuleDefault", () => {
  test("returns direct string action", () => {
    expect(getRuleDefault("allow")).toBe("allow")
    expect(getRuleDefault("ask")).toBe("ask")
    expect(getRuleDefault("deny")).toBe("deny")
  })

  test("returns wildcard action from object", () => {
    expect(getRuleDefault({ "*": "allow" })).toBe("allow")
    expect(getRuleDefault({ "*": "ask" })).toBe("ask")
    expect(getRuleDefault({ "*": "deny" })).toBe("deny")
  })

  test("returns undefined for missing or invalid values", () => {
    expect(getRuleDefault(undefined)).toBeUndefined()
    expect(getRuleDefault(null)).toBeUndefined()
    expect(getRuleDefault({})).toBeUndefined()
    expect(getRuleDefault({ foo: "allow" })).toBeUndefined()
  })
})

describe("toMap", () => {
  test("passes through an object as-is", () => {
    const obj = { read: "allow", edit: "ask" }
    expect(toMap(obj)).toEqual(obj)
  })

  test("wraps a string action into a wildcard map", () => {
    expect(toMap("allow")).toEqual({ "*": "allow" })
    expect(toMap("ask")).toEqual({ "*": "ask" })
    expect(toMap("deny")).toEqual({ "*": "deny" })
  })

  test("returns empty map for invalid/undefined values", () => {
    expect(toMap(undefined)).toEqual({})
    expect(toMap(null)).toEqual({})
    expect(toMap(42)).toEqual({})
    expect(toMap([])).toEqual({})
  })
})

describe("isYoloPermission", () => {
  test("returns true for string 'allow'", () => {
    expect(isYoloPermission("allow")).toBe(true)
  })

  test("returns true for { '*': 'allow' }", () => {
    expect(isYoloPermission({ "*": "allow" })).toBe(true)
  })

  test("returns true for the YOLO map produced by buildYoloPermissionMap", () => {
    expect(isYoloPermission(buildYoloPermissionMap())).toBe(true)
  })

  test("returns true when all per-tool overrides are also 'allow'", () => {
    expect(isYoloPermission({ "*": "allow", read: "allow", bash: "allow" })).toBe(true)
  })

  test("returns false when any per-tool override restricts access", () => {
    expect(isYoloPermission({ "*": "allow", bash: "ask" })).toBe(false)
    expect(isYoloPermission({ "*": "allow", edit: "deny" })).toBe(false)
  })

  test("returns false when wildcard is restrictive", () => {
    expect(isYoloPermission("ask")).toBe(false)
    expect(isYoloPermission("deny")).toBe(false)
    expect(isYoloPermission({ "*": "ask" })).toBe(false)
    expect(isYoloPermission({ "*": "deny" })).toBe(false)
  })

  test("returns false for empty or undefined permission", () => {
    expect(isYoloPermission(undefined)).toBe(false)
    expect(isYoloPermission(null)).toBe(false)
    expect(isYoloPermission({})).toBe(false)
  })

  test("returns false when wildcard is absent, even if all known tools are 'allow'", () => {
    // Without "*": "allow", unknown/future tools would not be covered.
    expect(isYoloPermission({ read: "allow", edit: "allow" })).toBe(false)
    const noWildcard = { ...buildYoloPermissionMap() }
    delete noWildcard["*"]
    expect(isYoloPermission(noWildcard)).toBe(false)
  })

  test("returns false when a per-tool sub-object restricts via its own wildcard", () => {
    expect(isYoloPermission({ "*": "allow", bash: { "*": "ask" } })).toBe(false)
    expect(isYoloPermission({ "*": "allow", edit: { "*": "deny" } })).toBe(false)
  })

  test("returns true when per-tool sub-objects all resolve to allow", () => {
    expect(isYoloPermission({ "*": "allow", bash: { "*": "allow" } })).toBe(true)
  })

  test("ignores array-valued entries (path lists) when checking for restrictions", () => {
    expect(isYoloPermission({ "*": "allow", bash: ["/safe/path"] })).toBe(true)
  })
})

describe("buildYoloPermissionMap", () => {
  test("sets wildcard and every known tool to allow", () => {
    const map = buildYoloPermissionMap()
    expect(map["*"]).toBe("allow")
    for (const id of ALL_TOOL_IDS) {
      expect(map[id]).toBe("allow")
    }
  })

  test("produces a map that isYoloPermission recognises as YOLO", () => {
    expect(isYoloPermission(buildYoloPermissionMap())).toBe(true)
  })

  test("contains exactly the wildcard plus ALL_TOOL_IDS keys (no extras, no missing)", () => {
    const map = buildYoloPermissionMap()
    const keys = new Set(Object.keys(map))
    expect(keys.has("*")).toBe(true)
    for (const id of ALL_TOOL_IDS) {
      expect(keys.has(id)).toBe(true)
    }
    expect(keys.size).toBe(ALL_TOOL_IDS.length + 1)
  })

  test("buildRestorePermissionMap covers every non-wildcard key written by buildYoloPermissionMap", () => {
    // Every per-tool key in the YOLO map must appear in the restore map so
    // mergeDeep can overwrite it on disable. The wildcard is intentionally not
    // restored when the saved config had none.
    const yoloMap = buildYoloPermissionMap()
    const restoreMap = buildRestorePermissionMap({})
    for (const key of Object.keys(yoloMap)) {
      if (key === "*") continue
      expect(restoreMap[key]).toBeDefined()
    }
  })
})

describe("buildRestorePermissionMap", () => {
  test("restores per-tool values from saved config", () => {
    const saved = { bash: "ask", edit: "deny", "*": "allow" }
    const map = buildRestorePermissionMap(saved)
    expect(map["bash"]).toBe("ask")
    expect(map["edit"]).toBe("deny")
    // tools not explicitly saved fall back to the saved wildcard
    expect(map["read"]).toBe("allow")
    // wildcard is restored because it was in the saved config
    expect(map["*"]).toBe("allow")
  })

  test("covers all tool IDs so mergeDeep overwrites every YOLO key", () => {
    const map = buildRestorePermissionMap({})
    // Every tool key must be present to overwrite the YOLO "allow" via mergeDeep.
    for (const id of ALL_TOOL_IDS) {
      expect(map[id]).toBeDefined()
    }
  })

  test("does not write wildcard when saved config had no wildcard", () => {
    // Critical: writing "*": "allow" unconditionally would override all
    // per-tool restrictions at evaluation time, breaking subsequent changes.
    const map = buildRestorePermissionMap({})
    expect(map["*"]).toBeUndefined()

    const mapFromUndefined = buildRestorePermissionMap(undefined)
    expect(mapFromUndefined["*"]).toBeUndefined()

    const mapFromPerTool = buildRestorePermissionMap({ bash: "ask" })
    expect(mapFromPerTool["*"]).toBeUndefined()
  })

  test("result is not recognised as YOLO when saved config was restrictive", () => {
    const saved = { bash: "ask", "*": "allow" }
    expect(isYoloPermission(buildRestorePermissionMap(saved))).toBe(false)
  })

  test("resets per-tool to 'ask' (opencode default) for empty saved config", () => {
    // Empty saved config means the user started with bare "*": "allow".
    // Tools with no saved value are reset to "ask" — opencode's built-in
    // default — so the user lands on system defaults after disabling YOLO.
    const map = buildRestorePermissionMap(undefined)
    for (const id of ALL_TOOL_IDS) {
      expect(map[id]).toBe("ask")
    }
    expect(map["*"]).toBeUndefined()
  })

  test("defaults per-tool to saved wildcard when no explicit per-tool entry", () => {
    const map = buildRestorePermissionMap({ "*": "ask" })
    expect(map["*"]).toBe("ask")
    for (const id of ALL_TOOL_IDS) {
      expect(map[id]).toBe("ask")
    }
  })

  // --- Additional edge case tests ---

  test("saved config as string 'ask': all tools default to ask, wildcard written", () => {
    // toMap("ask") => { "*": "ask" }, so savedWildcard is "ask".
    // Every tool should fall back to "ask" and the wildcard should be written.
    const map = buildRestorePermissionMap("ask")
    expect(map["*"]).toBe("ask")
    for (const id of ALL_TOOL_IDS) {
      expect(map[id]).toBe("ask")
    }
  })

  test("saved config as string 'allow': all tools set to allow, wildcard written", () => {
    const map = buildRestorePermissionMap("allow")
    expect(map["*"]).toBe("allow")
    for (const id of ALL_TOOL_IDS) {
      expect(map[id]).toBe("allow")
    }
  })

  test("saved config as string 'deny': all tools default to deny, wildcard written", () => {
    const map = buildRestorePermissionMap("deny")
    expect(map["*"]).toBe("deny")
    for (const id of ALL_TOOL_IDS) {
      expect(map[id]).toBe("deny")
    }
  })

  test("saved config as null: all tools reset to 'ask', no wildcard", () => {
    // null maps to {} via toMap, so no saved values — tools are reset to "ask".
    const map = buildRestorePermissionMap(null)
    expect(map["*"]).toBeUndefined()
    for (const id of ALL_TOOL_IDS) {
      expect(map[id]).toBe("ask")
    }
  })

  test("saved config with per-tool object value (bash sub-permissions): extracts default correctly", () => {
    // opencode supports per-sub-command permission objects, e.g. bash: { "*": "ask" }.
    // getRuleDefault should extract "ask" from the nested wildcard.
    const saved = { bash: { "*": "ask" }, edit: { "*": "deny" } }
    const map = buildRestorePermissionMap(saved)
    expect(map["bash"]).toBe("ask")
    expect(map["edit"]).toBe("deny")
    // No top-level wildcard was present.
    expect(map["*"]).toBeUndefined()
    // Unspecified tools are reset to "ask" (opencode default).
    expect(map["read"]).toBe("ask")
    expect(map["glob"]).toBe("ask")
  })

  test("saved config with per-tool object value with wildcard as top-level fallback", () => {
    // When both a top-level wildcard and per-tool sub-objects are present.
    const saved = { bash: { "*": "ask" }, "*": "deny" }
    const map = buildRestorePermissionMap(saved)
    expect(map["bash"]).toBe("ask")
    // Wildcard is "deny", so tools without explicit entries fall back to it.
    expect(map["read"]).toBe("deny")
    expect(map["edit"]).toBe("deny")
    // Top-level wildcard is written because it was in the saved config.
    expect(map["*"]).toBe("deny")
  })

  test("no wildcard guarantee: per-tool-only saved config never writes wildcard", () => {
    // Even with multiple per-tool entries, "*" must not appear unless it was saved.
    const saved = { read: "allow", edit: "deny", bash: "ask", glob: "deny" }
    const map = buildRestorePermissionMap(saved)
    expect(map["*"]).toBeUndefined()
    expect(map["read"]).toBe("allow")
    expect(map["edit"]).toBe("deny")
    expect(map["bash"]).toBe("ask")
    expect(map["glob"]).toBe("deny")
    // Tools not present in saved are reset to "ask" (opencode default).
    expect(map["grep"]).toBe("ask")
    expect(map["lsp"]).toBe("ask")
  })

  test("all ALL_TOOL_IDS are always present regardless of input shape", () => {
    // Crucial for mergeDeep to overwrite every key that buildYoloPermissionMap wrote.
    for (const input of [null, undefined, "ask", {}, { bash: "deny" }, { "*": "ask", bash: "deny" }]) {
      const map = buildRestorePermissionMap(input)
      for (const id of ALL_TOOL_IDS) {
        expect(map[id]).toBeDefined()
      }
    }
  })

  test("result is never YOLO when any tool has a non-allow saved value", () => {
    const saved = { bash: "deny", "*": "allow" }
    const map = buildRestorePermissionMap(saved)
    // bash is "deny" → overall config is not YOLO even though wildcard is "allow"
    expect(isYoloPermission(map)).toBe(false)
    expect(map["bash"]).toBe("deny")
  })

  test("result is not YOLO when saved was the initial '*': 'allow' (stripped to {} by createEffect)", () => {
    // The createEffect strips "*" before saving, so the saved config for an
    // initial "*": "allow" pod is {}. Restoring {} resets all tools to "ask"
    // (opencode's system default) with no wildcard — the user lands on the
    // same defaults as a pod with no permission config at all.
    const saved = {}
    const map = buildRestorePermissionMap(saved)
    expect(isYoloPermission(map)).toBe(false)
    expect(map["*"]).toBeUndefined()
    for (const id of ALL_TOOL_IDS) {
      expect(map[id]).toBe("ask")
    }
  })
})
