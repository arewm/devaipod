// Verify the Permissions-Policy header is present on HTML-serving routes.
//
// Without this header, browsers ignore allow="clipboard-read; clipboard-write"
// on cross-origin iframes, which blocks navigator.clipboard in the opencode UI.

import { test, expect } from "./fixtures"

test.describe("Permissions-Policy header", () => {
  test("agent iframe wrapper page sends clipboard policy", async ({
    page,
    devaipodUrl,
    devaipodToken,
    podShortNames,
  }) => {
    await page.goto(`${devaipodUrl}/_devaipod/login?token=${devaipodToken}`)

    let permissionsPolicy: string | null = null
    page.on("response", (response) => {
      if (response.url().includes("/_devaipod/agent/")) {
        permissionsPolicy = response.headers()["permissions-policy"] ?? null
      }
    })

    await page.goto(`${devaipodUrl}/_devaipod/agent/${podShortNames[0]}/`)
    await page.waitForSelector("#dbar", { timeout: 10_000 })

    expect(permissionsPolicy).toContain("clipboard-write")
    expect(permissionsPolicy).toContain("clipboard-read")
  })

  test("SPA index page sends clipboard policy", async ({
    page,
    devaipodUrl,
    devaipodToken,
  }) => {
    await page.goto(`${devaipodUrl}/_devaipod/login?token=${devaipodToken}`)

    let permissionsPolicy: string | null = null
    page.on("response", (response) => {
      if (response.url() === `${devaipodUrl}/pods`) {
        permissionsPolicy = response.headers()["permissions-policy"] ?? null
      }
    })

    await page.goto(`${devaipodUrl}/pods`)

    expect(permissionsPolicy).toContain("clipboard-write")
    expect(permissionsPolicy).toContain("clipboard-read")
  })
})
