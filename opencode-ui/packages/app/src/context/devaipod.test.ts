import { describe, expect, test } from "bun:test"
import {
  sortByRepo,
  sortByCreated,
  sortPods,
  groupPods,
  frecencySortPods,
  type PodInfo,
  type AgentStatus,
} from "./devaipod"

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makePod(overrides: Partial<PodInfo> & { Name: string }): PodInfo {
  return {
    Created: new Date().toISOString(),
    Status: "running",
    ...overrides,
  } as PodInfo
}

/** Inline copy of autoTitleFromTask (not exported from pods.tsx). */
function autoTitleFromTask(task: string): string {
  const trimmed = task.trim().replace(/\s+/g, " ")
  if (!trimmed) return ""
  const firstLine = trimmed.split(/[.\n]/)[0].trim()
  if (firstLine.length <= 50) return firstLine
  const cut = firstLine.slice(0, 50)
  const lastSpace = cut.lastIndexOf(" ")
  return lastSpace > 20 ? cut.slice(0, lastSpace) : cut
}

/** No-op agent status lookup — everything returns undefined. */
const noStatus = (_name: string): AgentStatus | undefined => undefined

// ---------------------------------------------------------------------------
// sortByRepo
// ---------------------------------------------------------------------------

describe("sortByRepo", () => {
  test("advisor pod always sorts first", () => {
    const pods = [
      makePod({ Name: "alpha", Labels: { "io.devaipod.repo": "aaa" } }),
      makePod({ Name: "devaipod-advisor" }),
      makePod({ Name: "beta", Labels: { "io.devaipod.repo": "bbb" } }),
    ]
    const sorted = sortByRepo(pods)
    expect(sorted[0].Name).toBe("devaipod-advisor")
  })

  test("sorts alphabetically by repo label", () => {
    const pods = [
      makePod({ Name: "p2", Labels: { "io.devaipod.repo": "zrepo" } }),
      makePod({ Name: "p1", Labels: { "io.devaipod.repo": "arepo" } }),
      makePod({ Name: "p3", Labels: { "io.devaipod.repo": "mrepo" } }),
    ]
    const sorted = sortByRepo(pods)
    expect(sorted.map((p) => p.Labels!["io.devaipod.repo"])).toEqual([
      "arepo",
      "mrepo",
      "zrepo",
    ])
  })

  test("running pods sort before stopped within the same repo", () => {
    const repo = "same-repo"
    const pods = [
      makePod({ Name: "stopped1", Status: "exited", Labels: { "io.devaipod.repo": repo } }),
      makePod({ Name: "running1", Status: "running", Labels: { "io.devaipod.repo": repo } }),
    ]
    const sorted = sortByRepo(pods)
    expect(sorted[0].Name).toBe("running1")
    expect(sorted[1].Name).toBe("stopped1")
  })

  test("pods without a repo label sort together", () => {
    const pods = [
      makePod({ Name: "labeled", Labels: { "io.devaipod.repo": "myrepo" } }),
      makePod({ Name: "unlabeled1" }),
      makePod({ Name: "unlabeled2" }),
    ]
    const sorted = sortByRepo(pods)
    // Empty string sorts before "myrepo", so unlabeled pods come first
    const unlabeled = sorted.filter((p) => !p.Labels?.["io.devaipod.repo"])
    expect(unlabeled).toHaveLength(2)
    // They should be adjacent
    const i1 = sorted.indexOf(unlabeled[0])
    const i2 = sorted.indexOf(unlabeled[1])
    expect(Math.abs(i1 - i2)).toBe(1)
  })
})

// ---------------------------------------------------------------------------
// sortByCreated
// ---------------------------------------------------------------------------

describe("sortByCreated", () => {
  test("advisor pod always sorts first", () => {
    const pods = [
      makePod({ Name: "new-pod", Created: "2026-01-02T00:00:00Z" }),
      makePod({ Name: "devaipod-advisor", Created: "2020-01-01T00:00:00Z" }),
    ]
    const sorted = sortByCreated(pods)
    expect(sorted[0].Name).toBe("devaipod-advisor")
  })

  test("sorts by Created timestamp descending (newest first)", () => {
    const pods = [
      makePod({ Name: "oldest", Created: "2024-01-01T00:00:00Z" }),
      makePod({ Name: "newest", Created: "2026-06-01T00:00:00Z" }),
      makePod({ Name: "middle", Created: "2025-06-01T00:00:00Z" }),
    ]
    const sorted = sortByCreated(pods)
    expect(sorted.map((p) => p.Name)).toEqual(["newest", "middle", "oldest"])
  })

  test("handles invalid Created gracefully", () => {
    const pods = [
      makePod({ Name: "valid", Created: "2025-06-01T00:00:00Z" }),
      makePod({ Name: "invalid", Created: "not-a-date" }),
    ]
    const sorted = sortByCreated(pods)
    // Invalid date gets timestamp 0 so valid comes first
    expect(sorted[0].Name).toBe("valid")
    expect(sorted[1].Name).toBe("invalid")
  })
})

// ---------------------------------------------------------------------------
// sortPods
// ---------------------------------------------------------------------------

describe("sortPods", () => {
  const pods = [
    makePod({ Name: "b", Created: "2025-01-01T00:00:00Z", Labels: { "io.devaipod.repo": "z" } }),
    makePod({ Name: "a", Created: "2026-01-01T00:00:00Z", Labels: { "io.devaipod.repo": "a" } }),
  ]

  test("'repo' dispatches to sortByRepo", () => {
    const result = sortPods(pods, "repo")
    expect(result).toEqual(sortByRepo(pods))
  })

  test("'created' dispatches to sortByCreated", () => {
    const result = sortPods(pods, "created")
    expect(result).toEqual(sortByCreated(pods))
  })

  test("'activity' dispatches to frecencySortPods", () => {
    const result = sortPods(pods, "activity")
    expect(result).toEqual(frecencySortPods(pods))
  })
})

// ---------------------------------------------------------------------------
// groupPods
// ---------------------------------------------------------------------------

describe("groupPods", () => {
  describe("none", () => {
    test("returns single group with empty label", () => {
      const pods = [makePod({ Name: "a" }), makePod({ Name: "b" })]
      const groups = groupPods(pods, "none", noStatus)
      expect(groups).toHaveLength(1)
      expect(groups[0].label).toBe("")
      expect(groups[0].pods).toHaveLength(2)
    })

    test("returns empty array for empty input", () => {
      expect(groupPods([], "none", noStatus)).toEqual([])
    })
  })

  describe("repo", () => {
    test("groups by io.devaipod.repo label", () => {
      const pods = [
        makePod({ Name: "p1", Labels: { "io.devaipod.repo": "repo-a" } }),
        makePod({ Name: "p2", Labels: { "io.devaipod.repo": "repo-b" } }),
        makePod({ Name: "p3", Labels: { "io.devaipod.repo": "repo-a" } }),
      ]
      const groups = groupPods(pods, "repo", noStatus)
      expect(groups.map((g) => g.label)).toEqual(["repo-a", "repo-b"])
      expect(groups[0].pods).toHaveLength(2)
      expect(groups[1].pods).toHaveLength(1)
    })

    test("pods without label go to 'Other'", () => {
      const pods = [
        makePod({ Name: "labeled", Labels: { "io.devaipod.repo": "myrepo" } }),
        makePod({ Name: "unlabeled" }),
      ]
      const groups = groupPods(pods, "repo", noStatus)
      const other = groups.find((g) => g.label === "Other")
      expect(other).toBeDefined()
      expect(other!.pods[0].Name).toBe("unlabeled")
    })
  })

  describe("status", () => {
    const cases: Array<{
      desc: string
      podStatus: string
      agentStatus: AgentStatus | undefined
      expectedGroup: string
    }> = [
      {
        desc: "running + Working -> Working",
        podStatus: "running",
        agentStatus: { activity: "Working" },
        expectedGroup: "Working",
      },
      {
        desc: "running + Idle -> Needs Attention",
        podStatus: "running",
        agentStatus: { activity: "Idle" },
        expectedGroup: "Needs Attention",
      },
      {
        desc: "running + Unknown -> Needs Attention",
        podStatus: "running",
        agentStatus: { activity: "Unknown" },
        expectedGroup: "Needs Attention",
      },
      {
        desc: "running + no agent status -> Needs Attention",
        podStatus: "running",
        agentStatus: undefined,
        expectedGroup: "Needs Attention",
      },
      {
        desc: "stopped -> Inactive",
        podStatus: "exited",
        agentStatus: { activity: "Stopped" },
        expectedGroup: "Inactive",
      },
      {
        desc: "running + done completion_status -> Inactive (edge case)",
        podStatus: "running",
        agentStatus: { activity: "Working", completion_status: "done" },
        expectedGroup: "Inactive",
      },
    ]

    for (const { desc, podStatus, agentStatus, expectedGroup } of cases) {
      test(desc, () => {
        const pod = makePod({ Name: "test-pod", Status: podStatus })
        const lookup = (name: string) => (name === "test-pod" ? agentStatus : undefined)
        const groups = groupPods([pod], "status", lookup)
        expect(groups).toHaveLength(1)
        expect(groups[0].label).toBe(expectedGroup)
      })
    }

    test("multiple pods distribute across all groups", () => {
      const pods = [
        makePod({ Name: "worker", Status: "running" }),
        makePod({ Name: "idle-one", Status: "running" }),
        makePod({ Name: "stopped-one", Status: "exited" }),
      ]
      const lookup = (name: string): AgentStatus | undefined => {
        if (name === "worker") return { activity: "Working" }
        if (name === "idle-one") return { activity: "Idle" }
        return undefined
      }
      const groups = groupPods(pods, "status", lookup)
      expect(groups.map((g) => g.label)).toEqual(["Working", "Needs Attention", "Inactive"])
    })
  })

  describe("time", () => {
    test("returns groups in correct order", () => {
      const now = Date.now()
      const pods = [
        makePod({ Name: "today", Created: new Date(now - 3600_000).toISOString() }),
        makePod({ Name: "this-week", Created: new Date(now - 3 * 86_400_000).toISOString() }),
        makePod({ Name: "this-month", Created: new Date(now - 14 * 86_400_000).toISOString() }),
        makePod({ Name: "older", Created: new Date(now - 60 * 86_400_000).toISOString() }),
      ]
      const groups = groupPods(pods, "time", noStatus)
      expect(groups.map((g) => g.label)).toEqual([
        "Active Today",
        "This Week",
        "This Month",
        "Older",
      ])
    })

    test("omits empty time sections", () => {
      const now = Date.now()
      const pods = [
        makePod({ Name: "today-only", Created: new Date(now - 1000).toISOString() }),
      ]
      const groups = groupPods(pods, "time", noStatus)
      expect(groups).toHaveLength(1)
      expect(groups[0].label).toBe("Active Today")
    })
  })
})

// ---------------------------------------------------------------------------
// autoTitleFromTask
// ---------------------------------------------------------------------------

describe("autoTitleFromTask", () => {
  const cases: Array<{ input: string; expected: string; desc: string }> = [
    { desc: "empty string", input: "", expected: "" },
    { desc: "whitespace only", input: "   \n  \t  ", expected: "" },
    { desc: "short task returned as-is", input: "Fix the login bug", expected: "Fix the login bug" },
    {
      desc: "trims surrounding whitespace",
      input: "  Fix the login bug  ",
      expected: "Fix the login bug",
    },
    {
      desc: "first sentence (before period) used if short",
      input: "Fix the login bug. Also update the docs.",
      expected: "Fix the login bug",
    },
    {
      desc: "first line (before newline) used if short",
      input: "Fix the login bug\nThis requires changing auth.ts",
      expected: "Fix the login bug",
    },
    {
      desc: "long task truncated at word boundary around 50 chars",
      input:
        "Implement the new authentication system with OAuth2 support and token refresh capabilities",
      expected: "Implement the new authentication system with",
    },
    {
      desc: "multiple whitespace collapsed",
      input: "Fix   the    login     bug",
      expected: "Fix the login bug",
    },
    {
      desc: "exactly 50 chars returned as-is",
      // 50 chars exactly: "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh ii"
      input: "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh ii",
      expected: "aaaaa bbbbb ccccc ddddd eeeee fffff ggggg hhhhh ii",
    },
    {
      desc: "no word boundary after position 20 falls back to hard cut",
      input: "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdef",
      expected: "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwx",
    },
  ]

  for (const { desc, input, expected } of cases) {
    test(desc, () => {
      expect(autoTitleFromTask(input)).toBe(expected)
    })
  }
})
