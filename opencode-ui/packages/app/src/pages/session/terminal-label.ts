export const terminalTabLabel = (input: {
  title?: string
  titleNumber?: number
  t: (key: string, vars?: Record<string, string | number | boolean>) => string
  kind?: "agent" | "workspace"
}) => {
  const title = input.title ?? ""
  const number = input.titleNumber ?? 0
  const match = title.match(/^Terminal (\d+)$/)
  const parsed = match ? Number(match[1]) : undefined
  const isDefaultTitle = Number.isFinite(number) && number > 0 && Number.isFinite(parsed) && parsed === number

  let label: string
  if (title && !isDefaultTitle) label = title
  else if (number > 0) label = input.t("terminal.title.numbered", { number })
  else if (title) label = title
  else label = input.t("terminal.title")

  if (input.kind === "agent") return `Agent: ${label}`
  if (input.kind === "workspace") return `Workspace: ${label}`
  return label
}
