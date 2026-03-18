import { test, expect } from "../fixtures"
import { closeDialog, openSettings } from "../actions"
import { settingsPermissionsYoloSelector } from "../selectors"

test("permissions tab renders tool list", async ({ page, gotoSession }) => {
  await gotoSession()

  const dialog = await openSettings(page)
  await dialog.getByRole("tab", { name: "Permissions" }).click()

  // The permissions heading should be visible
  await expect(dialog.getByRole("heading", { level: 2, name: "Permissions" })).toBeVisible()

  // The YOLO mode section heading should be present
  await expect(dialog.getByRole("heading", { level: 3, name: "YOLO Mode" })).toBeVisible()

  // The Tools section heading should be present
  await expect(dialog.getByRole("heading", { level: 3, name: "Tools" })).toBeVisible()

  // At minimum, the Bash and Edit tool rows should be visible
  await expect(dialog.getByText("Bash")).toBeVisible()
  await expect(dialog.getByText("Edit")).toBeVisible()

  await closeDialog(page, dialog)
})

test("YOLO mode toggle renders in permissions tab", async ({ page, gotoSession }) => {
  await gotoSession()

  const dialog = await openSettings(page)
  await dialog.getByRole("tab", { name: "Permissions" }).click()

  const yoloContainer = dialog.locator(settingsPermissionsYoloSelector)
  await expect(yoloContainer).toBeVisible()

  // YOLO should be off by default
  const toggleInput = yoloContainer.locator('[data-slot="switch-input"]')
  await expect(toggleInput).toHaveAttribute("aria-checked", "false")

  await closeDialog(page, dialog)
})

test("enabling YOLO mode updates config and shows as active", async ({ page, gotoSession, sdk }) => {
  await gotoSession()

  const dialog = await openSettings(page)
  await dialog.getByRole("tab", { name: "Permissions" }).click()

  const yoloContainer = dialog.locator(settingsPermissionsYoloSelector)
  await expect(yoloContainer).toBeVisible()

  const toggleInput = yoloContainer.locator('[data-slot="switch-input"]')
  const toggleControl = yoloContainer.locator('[data-slot="switch-control"]')

  // YOLO should start disabled
  await expect(toggleInput).toHaveAttribute("aria-checked", "false")

  // Enable YOLO
  await toggleControl.click()

  // Wait for the toggle to reflect enabled state
  await expect(toggleInput).toHaveAttribute("aria-checked", "true")

  // Verify the server config was updated to allow all tools
  const config = await sdk.config.get().then((r) => r.data)
  const permission = config?.permission
  expect(permission).toMatchObject({ "*": "allow" })

  await closeDialog(page, dialog)
})

test("disabling YOLO mode restores previous permissions", async ({ page, gotoSession, sdk }) => {
  await gotoSession()

  // Start with a specific permission config (bash: ask)
  await sdk.config.update({ body: { permission: { bash: "ask" } } })

  const dialog = await openSettings(page)
  await dialog.getByRole("tab", { name: "Permissions" }).click()

  const yoloContainer = dialog.locator(settingsPermissionsYoloSelector)
  const toggleInput = yoloContainer.locator('[data-slot="switch-input"]')
  const toggleControl = yoloContainer.locator('[data-slot="switch-control"]')

  // Enable YOLO
  await toggleControl.click()
  await expect(toggleInput).toHaveAttribute("aria-checked", "true")

  // Disable YOLO
  await toggleControl.click()
  await expect(toggleInput).toHaveAttribute("aria-checked", "false")

  // Verify the server config is no longer { "*": "allow" }
  await expect
    .poll(async () => {
      const config = await sdk.config.get().then((r) => r.data)
      const perm = config?.permission as Record<string, unknown> | undefined
      return perm?.["*"]
    })
    .not.toBe("allow")

  await closeDialog(page, dialog)
})

test("changing a tool permission via dropdown updates config", async ({ page, gotoSession, sdk }) => {
  await gotoSession()

  const dialog = await openSettings(page)
  await dialog.getByRole("tab", { name: "Permissions" }).click()

  // Find the Bash row — it should have a dropdown
  const bashRow = dialog.locator('[class*="border-b"]').filter({ hasText: "Bash" }).first()
  await expect(bashRow).toBeVisible()

  const trigger = bashRow.locator('[data-slot="select-select-trigger"]')
  await expect(trigger).toBeVisible()
  await trigger.click()

  // Select "Ask" from the dropdown
  const askItem = page.locator('[data-slot="select-select-item"]').filter({ hasText: "Ask" }).first()
  await expect(askItem).toBeVisible()
  await askItem.click()

  // Verify the server config was updated
  await expect
    .poll(async () => {
      const config = await sdk.config.get().then((r) => r.data)
      const perm = config?.permission as Record<string, unknown> | undefined
      return perm?.bash
    })
    .toBe("ask")

  await closeDialog(page, dialog)
})
