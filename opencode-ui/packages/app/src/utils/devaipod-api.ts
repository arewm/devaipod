/** Shared helpers for calling devaipod control plane APIs from the opencode SPA. */

export function getPodName(): string | undefined {
  const match = document.cookie.match(/(?:^|;\s*)DEVAIPOD_AGENT_POD=([^;]*)/)
  return match?.[1] ? decodeURIComponent(match[1]) : undefined
}

export function getAuthToken(): string | undefined {
  const stored = typeof sessionStorage !== "undefined" ? sessionStorage.getItem("devaipod_token") : null
  if (stored) return stored
  const params = new URLSearchParams(window.location.search)
  return params.get("token") ?? undefined
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
  return res.json()
}

/** True when the SPA is served through devaipod (cookie present). */
export function isDevaipod(): boolean {
  return document.cookie.includes("DEVAIPOD_AGENT_POD=")
}
