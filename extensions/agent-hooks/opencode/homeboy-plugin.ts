import type { Plugin } from "@opencode-ai/plugin"
import type { $ as Shell } from "bun"

// SYNC NOTE: Anti-pattern detection mirrors ../core/patterns.sh
// Context injection uses experimental.chat.system.transform hook.
// Topic detection and error suggestions are Claude Code-only (different hook APIs).

// Shell reference set during plugin initialization
let $: typeof Shell
// Cached init output for system prompt injection
let cachedInitOutput: string | null = null

/**
 * Execute homeboy command and return stdout
 * Gracefully returns empty on error (non-Homeboy repos)
 */
async function homeboy(args: string): Promise<string> {
  try {
    const result = await $`homeboy ${args}`.text()
    return result.trim()
  } catch {
    return ""
  }
}

/**
 * Get Homeboy init data for current project
 * Returns parsed JSON or null for non-Homeboy repos
 */
async function getHomeboyData(): Promise<Record<string, unknown> | null> {
  const output = await homeboy("init --json")
  if (!output) return null
  try {
    const parsed = JSON.parse(output)
    return parsed.data || null
  } catch {
    return null
  }
}

/**
 * Check if directory is monorepo root (contains components but no git_root)
 */
function isMonorepoRoot(data: Record<string, unknown>): boolean {
  const context = data?.context as Record<string, unknown> | undefined
  const gitRoot = context?.git_root
  const contained = (context?.contained_components as unknown[]) || []
  return !gitRoot && contained.length > 0
}

/**
 * Format component paths for display
 */
function formatComponentPaths(data: Record<string, unknown>): string {
  const components = (data?.components as Array<Record<string, unknown>>) || []
  if (components.length === 0) return ""
  return components
    .map((c) => `  ${c.id} → ${c.path || "unknown"}`)
    .join("\n")
}

// ============================================================================
// Anti-Pattern Detection (mirrors core/patterns.sh)
// ============================================================================

/**
 * Check for bash anti-patterns that should use Homeboy commands
 * Returns error message if anti-pattern detected, null otherwise
 *
 * Mirrors: claude/pre-tool-bash.sh + core/patterns.sh
 */
async function checkBashAntipatterns(command: string): Promise<string | null> {
  // Git commands: check for monorepo root or suggest homeboy
  if (/^git\s/.test(command)) {
    const data = await getHomeboyData()

    // Non-homeboy directory: pass through
    if (!data) return null

    // Check if in monorepo root (no git_root but has contained components)
    if (isMonorepoRoot(data)) {
      const paths = formatComponentPaths(data)
      return `Git Repository Not Found

You're in a project directory, not a git repository.
Component repositories available:
${paths}

Options:
  cd <component-path> && git <command>
  homeboy git <command> <component>
  homeboy changes <component>`
    }

    // In a git repo: check for git status anti-pattern
    if (/^git\s+status/.test(command)) {
      const paths = formatComponentPaths(data)
      const componentsSection = paths ? `Components:\n${paths}\n\n` : ""
      return `Use Homeboy for change detection:
  homeboy changes

${componentsSection}Benefits: Version context, changelog status, component-aware diffs`
    }

    return null
  }

  // Build script → homeboy build
  if (/^(\.\/(build\.sh)|bash\s+build\.sh|sh\s+build\.sh)/.test(command)) {
    return `Build Script Anti-Pattern

Use Homeboy for builds:
  homeboy build <component>

Benefits: Consistent build process, artifact management, validation`
  }

  // rsync to remote → homeboy deploy
  if (/^rsync.*@/.test(command)) {
    return `Deploy Anti-Pattern (rsync)

Use Homeboy for deployments:
  homeboy deploy

Benefits: Server configuration, artifact handling, post-deploy verification`
  }

  // scp to remote → homeboy deploy
  if (/^scp.*@/.test(command)) {
    return `Deploy Anti-Pattern (scp)

Use Homeboy for deployments:
  homeboy deploy

Benefits: Server configuration, artifact handling, post-deploy verification`
  }

  // npm version → homeboy version
  if (/^npm\s+version/.test(command)) {
    return `Version Anti-Pattern (npm)

Use Homeboy for version changes:
  homeboy version bump <component> patch|minor|major
  homeboy version set <component> X.Y.Z

Benefits: Automatic changelog, consistent targets, git commit`
  }

  // cargo set-version → homeboy version
  if (/^cargo\s+set-version/.test(command)) {
    return `Version Anti-Pattern (cargo)

Use Homeboy for version changes:
  homeboy version bump <component> patch|minor|major
  homeboy version set <component> X.Y.Z

Benefits: Automatic changelog, consistent targets, git commit`
  }

  return null
}

/**
 * Check if file path matches protected files from Homeboy config
 * Returns error message if protected, null otherwise
 *
 * Mirrors: claude/pre-tool-edit.sh
 */
async function checkProtectedFile(filePath: string): Promise<string | null> {
  const data = await getHomeboyData()
  if (!data) return null

  // Check changelog protection
  const changelogPath = (data.changelog as Record<string, unknown>)?.path as string | undefined
  if (changelogPath && filePath === changelogPath) {
    return `Changelog Protection

Use Homeboy for changelog entries:
  homeboy changelog add

This ensures proper formatting and version association.`
  }

  // Check version targets protection
  const versionTargets = ((data.version as Record<string, unknown>)?.targets as Array<Record<string, unknown>>) || []
  for (const target of versionTargets) {
    const targetPath = target.full_path as string | undefined
    if (targetPath && filePath === targetPath) {
      return `Version File Protection

Use Homeboy for version changes:
  homeboy version bump <component> patch|minor|major
  homeboy version set <component> X.Y.Z

Benefits: Automatic changelog, consistent targets, git commit`
    }
  }

  return null
}

/**
 * Homeboy Plugin for OpenCode
 * Enforces Homeboy usage patterns (anti-pattern blocking, file protection)
 */
export const HomeboyPlugin: Plugin = async (context) => {
  // Store shell reference for module functions
  $ = context.$

  // Run homeboy init and cache output for system prompt injection
  cachedInitOutput = await homeboy("init")

  return {
    // Inject homeboy context into system prompt
    "experimental.chat.system.transform": async (
      _input: { system: string },
      output: { system: string[] }
    ) => {
      if (cachedInitOutput) {
        output.system.push(`Homeboy Active (auto-init)\n\n${cachedInitOutput}`)
      }
    },

    // Before tool execution (mirrors PreToolUse hooks)
    "tool.execute.before": async (input, output) => {
      // Bash command validation (mirrors pre-tool-bash.sh)
      if (input.tool === "bash") {
        const command = output.args?.command as string
        if (command) {
          const violation = await checkBashAntipatterns(command)
          if (violation) {
            throw new Error(violation)
          }
        }
      }

      // File edit protection (mirrors pre-tool-edit.sh)
      if (input.tool === "write" || input.tool === "edit") {
        const filePath = (output.args?.filePath || output.args?.file_path) as string
        if (filePath) {
          const violation = await checkProtectedFile(filePath)
          if (violation) {
            throw new Error(violation)
          }
        }
      }
    },
  }
}
