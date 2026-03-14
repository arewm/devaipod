import { createSignal, createEffect, createMemo, onCleanup, onMount, Show, For } from "solid-js"
import { useParams, useNavigate, A } from "@solidjs/router"
import { DevaipodProvider, useDevaipod } from "@/context/devaipod"
import { apiFetch } from "@/utils/devaipod-api"

// ---------------------------------------------------------------------------
// Page wrapper — provides context (standalone, outside the OpenCode stack)
// ---------------------------------------------------------------------------

export default function AgentPage() {
  return (
    <DevaipodProvider>
      <AgentView />
    </DevaipodProvider>
  )
}

// ---------------------------------------------------------------------------
// Agent view — iframe wrapper with navigation bar
// ---------------------------------------------------------------------------

function AgentView() {
  const params = useParams()
  const navigate = useNavigate()
  const ctx = useDevaipod()

  // -- Name normalization ---------------------------------------------------

  const fullName = () => {
    const n = params.name
    return n.startsWith("devaipod-") ? n : `devaipod-${n}`
  }
  const shortName = () => params.name.replace(/^devaipod-/, "")

  // -- Iframe source discovery ----------------------------------------------

  const [iframeSrc, setIframeSrc] = createSignal("")
  const [loading, setLoading] = createSignal(true)
  const [error, setError] = createSignal("")

  createEffect(() => {
    const name = fullName()
    setLoading(true)
    setError("")
    apiFetch<{ url?: string; latest_session?: { id: string; directory: string } }>(
      `/api/devaipod/pods/${encodeURIComponent(name)}/opencode-info`,
    )
      .then((info) => {
        if (info.url) {
          // Replace hostname with current window hostname for remote access
          const url = new URL(info.url)
          url.hostname = window.location.hostname
          let src = url.toString()
          if (info.latest_session) {
            const dir = btoa(info.latest_session.directory)
            src = `${url.origin}/${encodeURIComponent(dir)}/session/${encodeURIComponent(info.latest_session.id)}`
          }
          setIframeSrc(src)
        }
        setLoading(false)
      })
      .catch((e) => {
        setError(`Could not connect to pod: ${e.message}`)
        setLoading(false)
      })
  })

  // -- Agent status & done state --------------------------------------------

  const agentStatus = () => ctx.agentStatus[fullName()]
  const isDone = () => agentStatus()?.completion_status === "done"
  const sessionTitle = () => agentStatus()?.title || ""

  // Update document title
  createEffect(() => {
    const title = sessionTitle()
    document.title = title ? `${title} — ${shortName()}` : `devaipod — ${shortName()}`
  })

  async function toggleDone() {
    const newStatus = isDone() ? "active" : "done"
    await apiFetch(`/api/devaipod/pods/${encodeURIComponent(fullName())}/completion-status`, {
      method: "PUT",
      body: JSON.stringify({ status: newStatus }),
    })
    ctx.refresh()
  }

  // Immediately fetch pod list so arrows & status are available without
  // waiting for the first poll interval.
  onMount(() => ctx.refresh())

  // -- Pod switcher ---------------------------------------------------------

  const [dropdownOpen, setDropdownOpen] = createSignal(false)

  const runningPods = createMemo(() => ctx.pods.filter((p) => p.Status.toLowerCase() === "running"))

  const currentIdx = () => runningPods().findIndex((p) => p.Name === fullName())
  const canPrev = () => currentIdx() > 0
  const canNext = () => currentIdx() >= 0 && currentIdx() < runningPods().length - 1

  function goPrev() {
    const idx = currentIdx()
    if (idx > 0) {
      const short = runningPods()[idx - 1].Name.replace(/^devaipod-/, "")
      navigate(`/agent/${encodeURIComponent(short)}`)
    }
  }

  function goNext() {
    const idx = currentIdx()
    if (idx >= 0 && idx < runningPods().length - 1) {
      const short = runningPods()[idx + 1].Name.replace(/^devaipod-/, "")
      navigate(`/agent/${encodeURIComponent(short)}`)
    }
  }

  function switchToPod(podName: string) {
    const short = podName.replace(/^devaipod-/, "")
    setDropdownOpen(false)
    navigate(`/agent/${encodeURIComponent(short)}`)
  }

  // Close dropdown on outside click
  let switcherRef: HTMLDivElement | undefined
  function handleOutsideClick(e: MouseEvent) {
    if (switcherRef && !switcherRef.contains(e.target as Node)) {
      setDropdownOpen(false)
    }
  }
  createEffect(() => {
    if (dropdownOpen()) {
      document.addEventListener("click", handleOutsideClick, true)
    } else {
      document.removeEventListener("click", handleOutsideClick, true)
    }
  })
  onCleanup(() => document.removeEventListener("click", handleOutsideClick, true))

  // Activity dot class for pod switcher items
  function podDotClass(podName: string): string {
    const status = ctx.agentStatus[podName]
    if (!status) return "bg-gray-500"
    if (status.completion_status === "done") return "bg-violet-400"
    switch (status.activity) {
      case "Working":
        return "bg-green-500 animate-[pulse-dot_1.5s_ease-in-out_infinite]"
      case "Idle":
        return "bg-blue-500"
      case "Stopped":
        return "bg-gray-500"
      default:
        return "bg-gray-500"
    }
  }

  function podStatusLabel(podName: string): string {
    const status = ctx.agentStatus[podName]
    if (!status) return ""
    if (status.completion_status === "done") return "done"
    return status.activity
  }

  // Display name for pod trigger button
  const triggerLabel = () => {
    const title = sessionTitle()
    return title || shortName()
  }

  return (
    <div class="h-full bg-background-base text-text-strong flex flex-col" style={{ overflow: "hidden" }}>
      {/* Navigation bar */}
      <div
        id="dbar"
        data-testid="agent-topbar"
        class="flex items-center px-3 gap-2 border-b border-border-base shrink-0"
        style={{ height: "44px" }}
      >
        <A
          href="/pods"
          class="text-text-strong no-underline text-sm font-medium px-3.5 py-1.5 rounded-md bg-fill-element-base border border-border-base hover:bg-fill-element-active hover:border-border-base transition-colors"
        >
          &larr; Pods
        </A>

        <button
          type="button"
          data-testid="done-btn"
          class="text-sm font-medium px-3.5 py-1.5 rounded-md border cursor-pointer transition-colors"
          classList={{
            "bg-[rgba(34,197,94,0.15)] border-[rgba(34,197,94,0.4)] text-[#86efac] hover:bg-[rgba(34,197,94,0.25)] hover:border-[rgba(34,197,94,0.6)]":
              isDone(),
            "bg-fill-element-base border-border-base text-text-strong hover:bg-fill-element-active hover:border-border-base":
              !isDone(),
          }}
          title="Mark this pod as done"
          onClick={toggleDone}
        >
          {isDone() ? "Done" : "Mark Done"}
        </button>

        <span class="flex-1" />

        {/* Pod switcher */}
        <div ref={switcherRef} class="flex items-center gap-0.5 relative">
          <button
            type="button"
            data-testid="prev-pod"
            class="px-2 py-1.5 text-base min-w-[30px] text-center text-text-strong bg-fill-element-base border border-border-base rounded-md cursor-pointer transition-colors hover:bg-fill-element-active disabled:opacity-30 disabled:cursor-default disabled:pointer-events-none"
            title="Previous pod"
            disabled={!canPrev()}
            onClick={goPrev}
          >
            &larr;
          </button>

          <button
            type="button"
            data-testid="pod-trigger"
            class="relative min-w-[140px] text-left pr-7 text-sm font-medium px-3.5 py-1.5 rounded-md bg-fill-element-base border border-border-base text-text-strong cursor-pointer transition-colors hover:bg-fill-element-active"
            title="Switch pod"
            onClick={() => setDropdownOpen((v) => !v)}
          >
            <span class="truncate block">{triggerLabel()}</span>
            <span class="absolute right-2.5 top-1/2 -translate-y-1/2 text-[11px] opacity-60">
              &#9662;
            </span>
          </button>

          <button
            type="button"
            data-testid="next-pod"
            class="px-2 py-1.5 text-base min-w-[30px] text-center text-text-strong bg-fill-element-base border border-border-base rounded-md cursor-pointer transition-colors hover:bg-fill-element-active disabled:opacity-30 disabled:cursor-default disabled:pointer-events-none"
            title="Next pod"
            disabled={!canNext()}
            onClick={goNext}
          >
            &rarr;
          </button>

          {/* Dropdown */}
          <Show when={dropdownOpen()}>
            <div data-testid="pod-dropdown" class="absolute top-full right-0 mt-1 min-w-[280px] max-h-[360px] overflow-y-auto bg-surface-base border border-border-base rounded-lg shadow-[0_8px_24px_rgba(0,0,0,0.5)] z-[100] p-1">
              <For each={runningPods()}>
                {(pod) => {
                  const podShort = () => pod.Name.replace(/^devaipod-/, "")
                  const podTitle = () => ctx.agentStatus[pod.Name]?.title
                  const isCurrent = () => pod.Name === fullName()
                  return (
                    <button
                      type="button"
                      data-testid="pod-item"
                      class="flex items-center gap-2 w-full text-left px-3 py-2 rounded-md text-[13px] text-text-strong border-none bg-transparent cursor-pointer transition-colors hover:bg-fill-element-base"
                      classList={{ "bg-fill-element-base font-semibold": isCurrent() }}
                      onClick={() => switchToPod(pod.Name)}
                    >
                      <span
                        class="w-2 h-2 rounded-full shrink-0"
                        classList={{ [podDotClass(pod.Name)]: true }}
                      />
                      <span class="flex-1 truncate">
                        {podTitle() || podShort()}
                      </span>
                      <Show when={podStatusLabel(pod.Name)}>
                        <span class="text-[11px] opacity-55 whitespace-nowrap">
                          {podStatusLabel(pod.Name)}
                        </span>
                      </Show>
                    </button>
                  )
                }}
              </For>
              <Show when={runningPods().length === 0}>
                <div class="px-3 py-2 text-[13px] opacity-50">No running pods</div>
              </Show>
            </div>
          </Show>
        </div>
      </div>

      {/* Content area */}
      <Show
        when={!loading()}
        fallback={
          <div class="flex-1 flex items-center justify-center">
            <div class="text-sm opacity-60">Connecting to pod...</div>
          </div>
        }
      >
        <Show
          when={!error()}
          fallback={
            <div class="flex-1 flex items-center justify-center">
              <div class="text-sm text-red-400 max-w-md text-center">{error()}</div>
            </div>
          }
        >
          <iframe
            src={iframeSrc()}
            class="w-full border-none"
            style={{ height: "calc(100% - 44px)" }}
            allow="clipboard-read; clipboard-write"
          />
        </Show>
      </Show>
    </div>
  )
}
