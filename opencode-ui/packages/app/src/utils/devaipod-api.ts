/** Shared helpers for calling devaipod APIs from the opencode SPA. */

/**
 * Set up frontend error reporting for devaipod.
 * Intercepts console.error/warn and POSTs them to /_devaipod/frontend-error
 * for correlation with server-side request traces.
 * Suppresses the harmless "[global-sdk] event stream error" from SSE aborts.
 */
export function initDevaipodErrorReporting(): void {
  if (!isDevaipod()) return

  const origError = console.error
  const origWarn = console.warn

  function report(level: string, args: unknown[]) {
    try {
      const msg = args
        .map((a) => (typeof a === "object" ? JSON.stringify(a) : String(a)))
        .join(" ")
      let stack = ""
      try {
        throw new Error()
      } catch (e) {
        stack = (e as Error).stack ?? ""
      }
      fetch("/_devaipod/frontend-error", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          message: `[${level}] ${msg}`,
          url: location.href,
          stack,
          context: navigator.userAgent,
        }),
      }).catch(() => {})
    } catch {
      /* best-effort */
    }
  }

  console.error = (...args: unknown[]) => {
    const first = args[0]
    if (typeof first === "string" && first.startsWith("[global-sdk] event stream error")) return
    origError.apply(console, args)
    report("error", args)
  }

  console.warn = (...args: unknown[]) => {
    origWarn.apply(console, args)
    report("warn", args)
  }

  window.addEventListener("unhandledrejection", (e) => {
    report("unhandledrejection", [e.reason])
  })
}

export function getAuthToken(): string | undefined {
  const stored = typeof sessionStorage !== "undefined" ? sessionStorage.getItem("devaipod_token") : null
  if (stored) return stored
  const params = new URLSearchParams(window.location.search)
  const token = params.get("token") ?? undefined
  // Persist to sessionStorage so navigations/redirects don't lose it
  if (token && typeof sessionStorage !== "undefined") {
    sessionStorage.setItem("devaipod_token", token)
  }
  return token
}

export async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const token = getAuthToken()
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    ...(init?.headers as Record<string, string> | undefined),
  }
  if (token) headers["Authorization"] = `Bearer ${token}`
  const res = await fetch(path, { ...init, headers })
  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText)
    throw new Error(`API ${res.status}: ${text}`)
  }
  const contentType = res.headers.get("content-type") ?? ""
  if (contentType.includes("application/json")) {
    return res.json()
  }
  // For non-JSON responses (e.g. 204 No Content), return undefined
  return undefined as T
}

/**
 * True when the SPA is running inside devaipod (built with VITE_DEVAIPOD=true).
 */
export function isDevaipod(): boolean {
  return import.meta.env.VITE_DEVAIPOD === "true"
}
