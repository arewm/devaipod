// SPA agent page integration tests.
//
// These tests validate the SolidJS agent page at /agent/:name,
// covering navigation, done-state toggling, and the pod switcher.
// They require two running pods (created by global-setup.ts).

import { test, expect } from "./fixtures"

test.describe("SPA agent page", () => {

  test("renders top bar with back button and done button", async ({
    page,
    gotoAgentSpa,
    podShortNames,
  }) => {
    await gotoAgentSpa(podShortNames[0])

    // Back button exists and links to /pods
    const backLink = page.locator('a[href="/pods"]')
    await expect(backLink).toBeVisible()

    // Done button exists
    const doneBtn = page.locator('[data-testid="done-btn"]')
    await expect(doneBtn).toBeVisible()

    // Iframe exists and has a src
    const iframe = page.locator('iframe')
    await expect(iframe).toBeVisible()
    const src = await iframe.getAttribute('src')
    expect(src).toBeTruthy()
  })

  test("done button toggles state", async ({
    page,
    gotoAgentSpa,
    podShortNames,
  }) => {
    await gotoAgentSpa(podShortNames[0])

    const doneBtn = page.locator('[data-testid="done-btn"]')
    await expect(doneBtn).toBeVisible()

    // Get initial text
    const initialText = await doneBtn.textContent()

    // Click to toggle
    await doneBtn.click()

    // Wait for text to change
    await page.waitForFunction(
      (initial) => {
        const btn = document.querySelector('[data-testid="done-btn"]')
        return btn && btn.textContent !== initial
      },
      initialText,
      { timeout: 5000 }
    )

    // Click again to toggle back
    await doneBtn.click()
    await page.waitForFunction(
      (initial) => {
        const btn = document.querySelector('[data-testid="done-btn"]')
        return btn && btn.textContent === initial
      },
      initialText,
      { timeout: 5000 }
    )
  })

  test("back button navigates to pods page", async ({
    page,
    gotoAgentSpa,
    podShortNames,
  }) => {
    await gotoAgentSpa(podShortNames[0])

    const backLink = page.locator('a[href="/pods"]')
    await backLink.click()

    // Should navigate to pods page
    await page.waitForURL(/\/pods/)
  })

  test("pod switcher shows running pods", async ({
    page,
    gotoAgentSpa,
    podShortNames,
  }) => {
    expect(podShortNames.length).toBeGreaterThanOrEqual(2)
    await gotoAgentSpa(podShortNames[0])

    // Click the pod trigger to open dropdown
    const trigger = page.locator('[data-testid="pod-trigger"]')
    await expect(trigger).toBeVisible()
    await trigger.click()

    // Dropdown should appear with pod items
    const dropdown = page.locator('[data-testid="pod-dropdown"]')
    await expect(dropdown).toBeVisible()

    // Should contain both test pods
    const items = dropdown.locator('[data-testid="pod-item"]')
    const count = await items.count()
    expect(count).toBeGreaterThanOrEqual(2)
  })

  test("pod switcher arrow navigation works", async ({
    page,
    gotoAgentSpa,
    podShortNames,
  }) => {
    expect(podShortNames.length).toBeGreaterThanOrEqual(2)
    await gotoAgentSpa(podShortNames[0])

    // Wait for pod list to load (arrows become enabled)
    const prevBtn = page.locator('[data-testid="prev-pod"]')
    const nextBtn = page.locator('[data-testid="next-pod"]')

    // At least one arrow should be enabled with 2+ pods
    await page.waitForFunction(() => {
      const prev = document.querySelector('[data-testid="prev-pod"]') as HTMLButtonElement
      const next = document.querySelector('[data-testid="next-pod"]') as HTMLButtonElement
      return (prev && !prev.disabled) || (next && !next.disabled)
    }, { timeout: 10_000 })

    // Click whichever is enabled
    const nextDisabled = await nextBtn.isDisabled()
    const btn = nextDisabled ? prevBtn : nextBtn

    const startUrl = page.url()
    await btn.click()

    // Should navigate to a different pod (SPA navigation, URL changes)
    await page.waitForURL(/\/agent\//, { timeout: 10_000 })
    // The URL should have changed to a different pod
    expect(page.url()).not.toBe(startUrl)
  })

})
