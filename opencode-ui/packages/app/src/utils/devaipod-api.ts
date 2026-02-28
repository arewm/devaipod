/** Shared helpers for calling devaipod control plane APIs from the opencode SPA. */

export function getPodName(): string | undefined {
  const match = document.cookie.match(/(?:^|;\s*)DEVAIPOD_AGENT_POD=([^;]*)/)
  return match?.[1] ? decodeURIComponent(match[1]) : undefined
}

/**
 * Scope localStorage keys per pod when running inside an agent iframe.
 * All agent iframes share the same origin, so without scoping one pod's
 * settings/session data would collide with another's.
 * Must be called before any localStorage access in the app.
 */
export function scopeLocalStorageToPod(): void {
  const pod = getPodName()
  if (!pod) return
  const prefix = `dpod:${pod}:`
  const origGet = localStorage.getItem.bind(localStorage)
  const origSet = localStorage.setItem.bind(localStorage)
  const origRemove = localStorage.removeItem.bind(localStorage)
  localStorage.getItem = (key: string) => origGet(prefix + key)
  localStorage.setItem = (key: string, value: string) => origSet(prefix + key, value)
  localStorage.removeItem = (key: string) => origRemove(prefix + key)
}

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
 * True when the SPA is running inside devaipod — either because it was
 * built with VITE_DEVAIPOD=true or because the DEVAIPOD_AGENT_POD cookie
 * is present (dev-mode fallback).
 */
export function isDevaipod(): boolean {
  if (import.meta.env.VITE_DEVAIPOD === "true") return true
  return document.cookie.includes("DEVAIPOD_AGENT_POD=")
}

/**
 * Base URL for the devaipod control plane API.
 * When running inside devaipod the SPA is served by the control plane,
 * so the origin is the API host. Returns empty string outside devaipod.
 */
export function getControlPlaneUrl(): string {
  if (!isDevaipod()) return ""
  return window.location.origin
}
