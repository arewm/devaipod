import { createEffect, createResource, on, Show, type JSX } from "solid-js"
import { createStore } from "solid-js/store"
import type { FileDiff } from "@opencode-ai/sdk/v2"
import { SessionReview } from "@opencode-ai/ui/session-review"
import { Select } from "@opencode-ai/ui/select"
import type { SelectedLineRange } from "@/context/file"
import type { LineComment } from "@/context/comments"
import { getPodName, apiFetch } from "@/utils/devaipod-api"

export type DiffStyle = "unified" | "split"

export interface GitReviewTabProps {
  diffStyle: DiffStyle
  onDiffStyleChange?: (style: DiffStyle) => void
  onLineComment?: (comment: { file: string; selection: SelectedLineRange; comment: string; preview?: string }) => void
  comments?: LineComment[]
  focusedComment?: { file: string; id: string } | null
  onFocusedCommentChange?: (focus: { file: string; id: string } | null) => void
  focusedFile?: string
  onScrollRef?: (el: HTMLDivElement) => void
  classes?: {
    root?: string
    header?: string
    container?: string
  }
}

interface GitLogEntry {
  sha: string
  short_sha: string
  message: string
  author: string
  author_email: string
  timestamp: string
  parents: string[]
}

interface ApiFileDiff {
  file: string
  before: string
  after: string
  additions: number
  deletions: number
  status: "added" | "deleted" | "modified"
}

function commitLabel(entry: GitLogEntry): string {
  const firstLine = entry.message.split("\n", 1)[0] ?? ""
  const subject = firstLine.length > 60 ? firstLine.slice(0, 57) + "..." : firstLine
  return `${entry.short_sha} ${subject}`
}

export function GitReviewTab(props: GitReviewTabProps) {
  const podName = getPodName()

  const [state, setState] = createStore({
    baseCommit: undefined as string | undefined,
  })

  const [log] = createResource(
    () => podName,
    async (pod) => {
      const data = await apiFetch<{ commits: GitLogEntry[] }>(
        `/api/devaipod/pods/${encodeURIComponent(pod)}/git/log`,
      )
      return data.commits
    },
  )

  // Default to the earliest commit when log loads
  createEffect(
    on(
      () => log(),
      (entries) => {
        if (!entries || entries.length === 0) return
        if (state.baseCommit !== undefined) return
        // The log is typically newest-first; pick the last entry as default base
        setState("baseCommit", entries[entries.length - 1]!.sha)
      },
    ),
  )

  const diffParams = () => {
    if (!podName) return undefined
    const entries = log()
    if (!entries || entries.length === 0) return undefined
    const base = state.baseCommit
    if (!base) return undefined
    const head = entries[0]!.sha
    if (base === head) return undefined
    return { base, head, pod: podName }
  }

  const [diffData] = createResource(diffParams, async (params) => {
    const data = await apiFetch<{ files: ApiFileDiff[] }>(
      `/api/devaipod/pods/${encodeURIComponent(params.pod)}/git/diff-range?base=${encodeURIComponent(params.base)}&head=${encodeURIComponent(params.head)}`,
    )
    return data.files
  })

  const diffs = (): FileDiff[] => {
    const files = diffData()
    if (!files) return []
    return files.map((f) => ({
      file: f.file,
      before: f.before,
      after: f.after,
      additions: f.additions,
      deletions: f.deletions,
      status: f.status,
    }))
  }

  const commitOptions = () => {
    const entries = log()
    if (!entries) return []
    return entries
  }

  const title = (): JSX.Element => (
    <div class="flex items-center gap-3">
      <span>Changes</span>
      <Show when={commitOptions().length > 1}>
        <span class="text-text-weak text-13-regular">from</span>
        <Select
          options={commitOptions()}
          current={commitOptions().find((e) => e.sha === state.baseCommit)}
          value={(e) => e.sha}
          label={(e) => commitLabel(e)}
          onSelect={(entry) => entry && setState("baseCommit", entry.sha)}
          variant="ghost"
          size="small"
        />
      </Show>
    </div>
  )

  const loading = () => log.loading || diffData.loading
  const error = () => log.error ?? diffData.error

  return (
    <Show
      when={podName}
      fallback={
        <div class="flex h-full items-center justify-center text-text-weak text-14-regular">
          Not connected to a devaipod pod
        </div>
      }
    >
      <Show
        when={!error()}
        fallback={
          <div class="flex h-full items-center justify-center text-text-danger text-14-regular">
            Failed to load git data: {String(error())}
          </div>
        }
      >
        <Show
          when={!loading() || diffs().length > 0}
          fallback={
            <div class="flex h-full items-center justify-center text-text-weak text-14-regular">Loading...</div>
          }
        >
          <SessionReview
            title={title()}
            scrollRef={(el) => props.onScrollRef?.(el)}
            classes={{
              root: props.classes?.root ?? "pb-6",
              header: props.classes?.header ?? "px-6",
              container: props.classes?.container ?? "px-6",
            }}
            diffs={diffs()}
            diffStyle={props.diffStyle}
            onDiffStyleChange={props.onDiffStyleChange}
            focusedFile={props.focusedFile}
            onLineComment={props.onLineComment}
            comments={props.comments}
            focusedComment={props.focusedComment}
            onFocusedCommentChange={props.onFocusedCommentChange}
          />
        </Show>
      </Show>
    </Show>
  )
}
