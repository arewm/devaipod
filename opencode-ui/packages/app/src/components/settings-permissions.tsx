import { Select } from "@opencode-ai/ui/select"
import { Switch } from "@opencode-ai/ui/switch"
import { showToast } from "@opencode-ai/ui/toast"
import { Component, For, createEffect, createMemo, type JSX } from "solid-js"
import { createStore } from "solid-js/store"
import { useGlobalSync } from "@/context/global-sync"
import { useLanguage } from "@/context/language"
import { Persist, persisted } from "@/utils/persist"

type PermissionAction = "allow" | "ask" | "deny"

type PermissionObject = Record<string, PermissionAction>
type PermissionValue = PermissionAction | PermissionObject | string[] | undefined
type PermissionMap = Record<string, PermissionValue>

type PermissionItem = {
  id: string
  title: string
  description: string
}

const ACTIONS = [
  { value: "allow", label: "settings.permissions.action.allow" },
  { value: "ask", label: "settings.permissions.action.ask" },
  { value: "deny", label: "settings.permissions.action.deny" },
] as const

const ITEMS = [
  {
    id: "read",
    title: "settings.permissions.tool.read.title",
    description: "settings.permissions.tool.read.description",
  },
  {
    id: "edit",
    title: "settings.permissions.tool.edit.title",
    description: "settings.permissions.tool.edit.description",
  },
  {
    id: "glob",
    title: "settings.permissions.tool.glob.title",
    description: "settings.permissions.tool.glob.description",
  },
  {
    id: "grep",
    title: "settings.permissions.tool.grep.title",
    description: "settings.permissions.tool.grep.description",
  },
  {
    id: "list",
    title: "settings.permissions.tool.list.title",
    description: "settings.permissions.tool.list.description",
  },
  {
    id: "bash",
    title: "settings.permissions.tool.bash.title",
    description: "settings.permissions.tool.bash.description",
  },
  {
    id: "task",
    title: "settings.permissions.tool.task.title",
    description: "settings.permissions.tool.task.description",
  },
  {
    id: "skill",
    title: "settings.permissions.tool.skill.title",
    description: "settings.permissions.tool.skill.description",
  },
  {
    id: "lsp",
    title: "settings.permissions.tool.lsp.title",
    description: "settings.permissions.tool.lsp.description",
  },
  {
    id: "todoread",
    title: "settings.permissions.tool.todoread.title",
    description: "settings.permissions.tool.todoread.description",
  },
  {
    id: "todowrite",
    title: "settings.permissions.tool.todowrite.title",
    description: "settings.permissions.tool.todowrite.description",
  },
  {
    id: "webfetch",
    title: "settings.permissions.tool.webfetch.title",
    description: "settings.permissions.tool.webfetch.description",
  },
  {
    id: "websearch",
    title: "settings.permissions.tool.websearch.title",
    description: "settings.permissions.tool.websearch.description",
  },
  {
    id: "codesearch",
    title: "settings.permissions.tool.codesearch.title",
    description: "settings.permissions.tool.codesearch.description",
  },
  {
    id: "external_directory",
    title: "settings.permissions.tool.external_directory.title",
    description: "settings.permissions.tool.external_directory.description",
  },
  {
    id: "doom_loop",
    title: "settings.permissions.tool.doom_loop.title",
    description: "settings.permissions.tool.doom_loop.description",
  },
] as const

const VALID_ACTIONS = new Set<PermissionAction>(["allow", "ask", "deny"])

// All tool IDs that the UI manages. Used to build complete permission maps for
// YOLO enable/disable so that mergeDeep (which can only add/overwrite, not
// remove keys) properly overwrites any previously-set per-tool values.
export const ALL_TOOL_IDS = ITEMS.map((item) => item.id)

/**
 * Builds the permission map to write when enabling YOLO mode.
 * Every known tool is explicitly set to "allow" AND the wildcard "*" is set to
 * "allow". The wildcard is required to cover any tools not in ALL_TOOL_IDS
 * (e.g. tools added in future opencode versions). Without it, unknown tools
 * would fall back to opencode's built-in default ("ask"), which is not YOLO.
 */
export function buildYoloPermissionMap(): PermissionMap {
  const map: PermissionMap = { "*": "allow" }
  for (const id of ALL_TOOL_IDS) map[id] = "allow"
  return map
}

/**
 * Builds the permission map to write when disabling YOLO mode, given the
 * previously saved permission config. Every key written by buildYoloPermissionMap
 * is explicitly overwritten so that mergeDeep removes all YOLO values.
 *
 * Only tools that were explicitly set in the saved config are written back —
 * tools with no saved value are omitted so that opencode's built-in defaults
 * apply naturally. The wildcard "*" is only written if it was in the saved
 * config, since writing "*": "allow" unconditionally would override all
 * per-tool restrictions at evaluation time.
 *
 * If the saved config is empty (e.g. the user started with a bare "*": "allow"
 * pod and the createEffect stripped the wildcard), the result is an empty-ish
 * map: every YOLO key is overwritten with the opencode default, and the user
 * lands on system defaults (typically "ask" for each tool).
 */
export function buildRestorePermissionMap(saved: unknown): PermissionMap {
  const savedMap = toMap(saved)
  const savedWildcard = getRuleDefault(savedMap["*"])
  const map: PermissionMap = {}
  for (const id of ALL_TOOL_IDS) {
    const savedValue = getRuleDefault(savedMap[id]) ?? savedWildcard
    // Every key must be present to overwrite the YOLO "allow" via mergeDeep.
    // If the tool had no saved value, use "ask" (opencode's built-in default).
    map[id] = savedValue ?? "ask"
  }
  // Only restore the wildcard if it was explicitly in the saved config.
  if (savedWildcard !== undefined) {
    map["*"] = savedWildcard
  }
  return map
}

export function toMap(value: unknown): PermissionMap {
  if (value && typeof value === "object" && !Array.isArray(value)) return value as PermissionMap

  const action = getAction(value)
  if (action) return { "*": action }

  return {}
}

export function getAction(value: unknown): PermissionAction | undefined {
  if (typeof value === "string" && VALID_ACTIONS.has(value as PermissionAction)) return value as PermissionAction
  return
}

export function getRuleDefault(value: unknown): PermissionAction | undefined {
  const action = getAction(value)
  if (action) return action

  if (!value || typeof value !== "object" || Array.isArray(value)) return

  return getAction((value as Record<string, unknown>)["*"])
}

/**
 * Returns true if the given permission config represents "allow all" (YOLO mode).
 *
 * Requires "*": "allow" to be present — this is the invariant that covers
 * unknown tools (those not in ALL_TOOL_IDS). A config with only explicit
 * per-tool "allow" entries is not YOLO because future/unknown tools would fall
 * back to opencode's default ("ask") rather than being allowed.
 */
export function isYoloPermission(permission: unknown): boolean {
  const map = toMap(permission)

  // Wildcard must be "allow" — this is what protects unknown tools.
  const wildcard = getRuleDefault(map["*"])
  if (wildcard !== "allow") return false

  // No known tool may have an explicit restriction that overrides the wildcard.
  for (const id of ALL_TOOL_IDS) {
    const action = getRuleDefault(map[id])
    if (action && action !== "allow") return false
  }

  return true
}

export const SettingsPermissions: Component = () => {
  const globalSync = useGlobalSync()
  const language = useLanguage()

  // Persisted store for saved pre-YOLO permissions, scoped globally (pod-level).
  const [yoloStore, setYoloStore] = persisted(
    Persist.global("permissions.yolo", ["permissions.yolo.v1"]),
    createStore({
      // The serialized pre-YOLO permission config. null means YOLO is not active.
      savedPermission: null as unknown,
    }),
  )

  const actions = createMemo(
    (): Array<{ value: PermissionAction; label: string }> =>
      ACTIONS.map((action) => ({
        value: action.value,
        label: language.t(action.label),
      })),
  )

  const permission = createMemo(() => {
    return toMap(globalSync.data.config.permission)
  })

  const yoloActive = createMemo(() => {
    return yoloStore.savedPermission !== null && isYoloPermission(globalSync.data.config.permission)
  })

  // If the config has "*": "allow" at load time (e.g. set via OPENCODE_PERMISSION
  // or opencode.json), automatically activate YOLO mode. This normalises the
  // wildcard into explicit per-tool entries and records the pre-YOLO state so
  // the user can disable it cleanly. The savedPermission guard makes this
  // idempotent — it only fires when YOLO is not already active.
  createEffect(() => {
    const perm = globalSync.data.config.permission
    if (yoloStore.savedPermission !== null) return
    if (!isYoloPermission(perm)) return
    // Save the config without the wildcard as the restore point.
    const savedMap = toMap(perm)
    const withoutWildcard: PermissionMap = {}
    for (const [key, value] of Object.entries(savedMap)) {
      if (key !== "*") withoutWildcard[key] = value
    }
    setYoloStore("savedPermission", withoutWildcard)
    // Write the normalised YOLO map so "*" is removed from the server config.
    void globalSync.updateConfig({ permission: buildYoloPermissionMap() })
  })

  const actionFor = (id: string): PermissionAction => {
    const value = permission()[id]
    const direct = getRuleDefault(value)
    if (direct) return direct

    const wildcard = getRuleDefault(permission()["*"])
    if (wildcard) return wildcard

    return "allow"
  }

  const setPermission = async (id: string, action: PermissionAction) => {
    const before = globalSync.data.config.permission
    const map = toMap(before)
    const existing = map[id]

    const nextValue =
      existing && typeof existing === "object" && !Array.isArray(existing) ? { ...existing, "*": action } : action

    const rollback = (err: unknown) => {
      globalSync.set("config", "permission", before)
      const message = err instanceof Error ? err.message : String(err)
      showToast({ title: language.t("settings.permissions.toast.updateFailed.title"), description: message })
    }

    globalSync.set("config", "permission", { ...map, [id]: nextValue })
    globalSync.updateConfig({ permission: { [id]: nextValue } }).catch(rollback)
  }

  const enableYolo = async () => {
    const current = globalSync.data.config.permission
    // Save current permission before enabling YOLO.
    setYoloStore("savedPermission", current ?? {})

    const rollback = (err: unknown) => {
      setYoloStore("savedPermission", null)
      const message = err instanceof Error ? err.message : String(err)
      showToast({ title: language.t("settings.permissions.toast.updateFailed.title"), description: message })
    }

    await globalSync.updateConfig({ permission: buildYoloPermissionMap() }).catch(rollback)

    showToast({
      variant: "success",
      icon: "circle-check",
      title: language.t("settings.permissions.toast.yoloEnabled.title"),
      description: language.t("settings.permissions.toast.yoloEnabled.description"),
    })
  }

  const disableYolo = async () => {
    const saved = yoloStore.savedPermission
    // Clear saved state before restoring so even on error we exit YOLO mode.
    setYoloStore("savedPermission", null)

    const rollback = (err: unknown) => {
      const message = err instanceof Error ? err.message : String(err)
      showToast({ title: language.t("settings.permissions.toast.updateFailed.title"), description: message })
    }

    await globalSync.updateConfig({ permission: buildRestorePermissionMap(saved) }).catch(rollback)

    showToast({
      title: language.t("settings.permissions.toast.yoloDisabled.title"),
      description: language.t("settings.permissions.toast.yoloDisabled.description"),
    })
  }

  const toggleYolo = (checked: boolean) => {
    if (checked) {
      void enableYolo()
    } else {
      void disableYolo()
    }
  }

  return (
    <div class="flex flex-col h-full overflow-y-auto no-scrollbar">
      <div class="sticky top-0 z-10 bg-[linear-gradient(to_bottom,var(--surface-raised-stronger-non-alpha)_calc(100%_-_24px),transparent)]">
        <div class="flex flex-col gap-1 px-4 py-8 sm:p-8 max-w-[720px]">
          <h2 class="text-16-medium text-text-strong">{language.t("settings.permissions.title")}</h2>
          <p class="text-14-regular text-text-weak">{language.t("settings.permissions.description")}</p>
        </div>
      </div>

      <div class="flex flex-col gap-6 px-4 py-6 sm:p-8 sm:pt-6 max-w-[720px]">
        <div class="flex flex-col gap-2">
          <h3 class="text-14-medium text-text-strong">{language.t("settings.permissions.section.yolo")}</h3>
          <div class="border border-border-weak-base rounded-lg overflow-hidden">
            <SettingsRow
              title={language.t("settings.permissions.yolo.title")}
              description={language.t("settings.permissions.yolo.description")}
            >
              <div data-action="settings-permissions-yolo">
                <Switch checked={yoloActive()} onChange={toggleYolo} />
              </div>
            </SettingsRow>
          </div>
        </div>

        <div class="flex flex-col gap-2" classList={{ "opacity-50 pointer-events-none": yoloActive() }}>
          <h3 class="text-14-medium text-text-strong">{language.t("settings.permissions.section.tools")}</h3>
          <div class="border border-border-weak-base rounded-lg overflow-hidden">
            <For each={ITEMS}>
              {(item) => (
                <SettingsRow title={language.t(item.title)} description={language.t(item.description)}>
                  <Select
                    options={actions()}
                    current={actions().find((o) => o.value === actionFor(item.id))}
                    value={(o) => o.value}
                    label={(o) => o.label}
                    onSelect={(option) => option && setPermission(item.id, option.value)}
                    disabled={yoloActive()}
                    variant="secondary"
                    size="small"
                    triggerVariant="settings"
                  />
                </SettingsRow>
              )}
            </For>
          </div>
        </div>
      </div>
    </div>
  )
}

interface SettingsRowProps {
  title: string
  description: string
  children: JSX.Element
}

const SettingsRow: Component<SettingsRowProps> = (props) => {
  return (
    <div class="flex flex-wrap items-center justify-between gap-4 px-4 py-3 border-b border-border-weak-base last:border-none">
      <div class="flex flex-col gap-0.5 min-w-0">
        <span class="text-14-medium text-text-strong">{props.title}</span>
        <span class="text-12-regular text-text-weak">{props.description}</span>
      </div>
      <div class="flex-shrink-0">{props.children}</div>
    </div>
  )
}
