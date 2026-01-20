import type { Plugin } from "@opencode-ai/plugin"
import { exec } from "child_process"
import { promisify } from "util"

const execAsync = promisify(exec)

/**
 * Execute homeboy command and return stdout
 * Gracefully returns empty on error (non-Homeboy repos)
 */
async function homeboy(args: string): Promise<string> {
  try {
    const { stdout } = await execAsync(`homeboy ${args}`)
    return stdout.trim()
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
 * Check for bash anti-patterns that should use Homeboy commands
 * Returns error message if anti-pattern detected, null otherwise
 *
 * Mirrors: claude/pre-tool-bash.sh + core/patterns.sh
 */
function checkBashAntipatterns(command: string): string | null {
  // Git status → homeboy changes
  if (/^git\s+status/.test(command)) {
    return `Git Status Anti-Pattern

Use Homeboy for change detection:
  homeboy changes

Benefits: Shows version context, changelog status, component-aware diffs`
  }

  // Build script → homeboy build
  if (/(\.\/(build\.sh)|bash\s+build\.sh|sh\s+build\.sh)/.test(command)) {
    return `Build Script Anti-Pattern

Use Homeboy for builds:
  homeboy build <component>

Benefits: Consistent build process, artifact management, validation`
  }

  // rsync to remote → homeboy deploy
  if (/rsync.*@/.test(command)) {
    return `Deploy Anti-Pattern (rsync)

Use Homeboy for deployments:
  homeboy deploy

Benefits: Server configuration, artifact handling, post-deploy verification`
  }

  // scp to remote → homeboy deploy
  if (/scp.*@/.test(command)) {
    return `Deploy Anti-Pattern (scp)

Use Homeboy for deployments:
  homeboy deploy

Benefits: Server configuration, artifact handling, post-deploy verification`
  }

  // npm version → homeboy version
  if (/npm\s+version/.test(command)) {
    return `Version Anti-Pattern (npm)

Use Homeboy for version changes:
  homeboy version bump <component> patch|minor|major
  homeboy version set <component> X.Y.Z

Benefits: Automatic changelog, consistent targets, git commit`
  }

  // cargo set-version → homeboy version
  if (/cargo\s+set-version/.test(command)) {
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
 * Enforces Homeboy usage patterns with full Claude Code parity
 */
export const HomeboyPlugin: Plugin = async () => {
  // Note: client.app.log() causes blank screen - OpenCode bug?
  // Session start message handled via tool hooks instead

  return {
    // Before tool execution (mirrors PreToolUse hooks)
    "tool.execute.before": async (input, output) => {
      // Bash command validation (mirrors pre-tool-bash.sh)
      if (input.tool === "bash") {
        const command = output.args?.command as string
        if (command) {
          const violation = checkBashAntipatterns(command)
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

    // After tool execution (PostToolUse equivalent - for future use)
    "tool.execute.after": async (_input, _output) => {
      // Reserved for future logging/metrics
    },

    // Event handler for session events
    event: async ({ event }) => {
      // Session idle notification (Stop equivalent)
      if (event.type === "session.idle") {
        // Could trigger homeboy notifications here
      }
    },
  }
}
