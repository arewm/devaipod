type AgentModel = {
  providerID: string
  modelID: string
}

type Agent = {
  model?: AgentModel
  variant?: string
}

type Model = AgentModel & {
  variants?: Record<string, unknown>
}

type VariantInput = {
  variants: string[]
  selected: string | undefined
  configured: string | undefined
}

export function getConfiguredAgentVariant(input: { agent: Agent | undefined; model: Model | undefined }) {
  if (!input.agent?.variant) return undefined
  if (!input.agent.model) return undefined
  if (!input.model?.variants) return undefined
  if (input.agent.model.providerID !== input.model.providerID) return undefined
  if (input.agent.model.modelID !== input.model.modelID) return undefined
  if (!(input.agent.variant in input.model.variants)) return undefined
  return input.agent.variant
}

export function resolveModelVariant(input: VariantInput) {
  if (input.selected && input.variants.includes(input.selected)) return input.selected
  if (input.configured && input.variants.includes(input.configured)) return input.configured
  return undefined
}

/**
 * Fuzzy-match a configured model ID against a provider's model map.
 *
 * When config specifies e.g. `claude-sonnet-4@default` but the provider
 * only has `claude-sonnet-4@20250514`, this strips the `@variant` suffix
 * and matches by base name. Returns the actual model ID or undefined.
 */
export function fuzzyMatchModelID(configuredModelID: string, availableModelIDs: string[]): string | undefined {
  // Exact match
  if (availableModelIDs.includes(configuredModelID)) return configuredModelID

  const atIndex = configuredModelID.indexOf("@")
  if (atIndex <= 0) return undefined

  const baseName = configuredModelID.substring(0, atIndex)
  // Exact base name (no variant suffix at all)
  if (availableModelIDs.includes(baseName)) return baseName
  // Any model sharing the same base
  return availableModelIDs.find((id) => id.startsWith(baseName + "@"))
}

export function cycleModelVariant(input: VariantInput) {
  if (input.variants.length === 0) return undefined
  if (input.selected && input.variants.includes(input.selected)) {
    const index = input.variants.indexOf(input.selected)
    if (index === input.variants.length - 1) return undefined
    return input.variants[index + 1]
  }
  if (input.configured && input.variants.includes(input.configured)) {
    const index = input.variants.indexOf(input.configured)
    if (index === input.variants.length - 1) return input.variants[0]
    return input.variants[index + 1]
  }
  return input.variants[0]
}
