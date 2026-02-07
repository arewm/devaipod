//! System prompt generation for AI agents in devaipod
//!
//! This module provides functions to generate context-aware system prompts
//! and instructions for AI agents, including orchestration-specific instructions
//! for the task owner agent in multi-agent mode.

use crate::pod::WORKER_OPENCODE_PORT;

/// Generate orchestration-specific instructions for the task owner agent.
///
/// These instructions explain to the task owner:
/// - That it MUST delegate implementation to the worker agent
/// - How to communicate with the worker via the OpenCode API
/// - That it MUST fetch and review worker's commits
/// - That it should only accept valid incremental progress
/// - Its role as reviewer and final committer
pub fn orchestration_instructions() -> String {
    format!(
        r#"
## Multi-Agent Orchestration - MANDATORY

You are the **task owner** in a multi-agent orchestration setup. You have a **worker agent**
that you **MUST** use to delegate implementation work.

### CRITICAL: You MUST Follow This Workflow

**You are a reviewer and orchestrator, NOT an implementer.** Do NOT write code or make
changes yourself. Your job is to:

1. **MUST: Delegate to worker** - Send implementation tasks to the worker agent
2. **MUST: Fetch and review commits** - After worker completes, fetch via git and review
3. **MUST: Only accept valid progress** - Reject or iterate if commits are incomplete/wrong
4. **MUST: Merge only after review** - Cherry-pick or merge validated commits
5. **MUST: Create PR when complete** - Use service-gator to create the final PR

### Worker Connection

The environment variable `OPENCODE_WORKER_URL` is set to `http://localhost:{worker_port}`.
Use `opencode run --attach --format json` to delegate tasks to the worker.

### Step 1: Delegate to Worker (REQUIRED)

Use `opencode run --attach` to send tasks to the worker. The `--format json` flag outputs
machine-readable ndjson events. The command blocks until the worker completes.

```bash
# Delegate a task to the worker (blocks until worker completes)
opencode run --attach "$OPENCODE_WORKER_URL" --format json "YOUR SPECIFIC TASK HERE. When done, commit your changes with a descriptive message."
```

The command streams JSON events and returns when the worker finishes. Exit code 0 means success.

**For continuing a session** (e.g., to send feedback):

```bash
# Continue the same session with follow-up instructions
opencode run --attach "$OPENCODE_WORKER_URL" --format json --continue "Your follow-up message or feedback"
```

### Step 2: Fetch and Review Commits (REQUIRED)

After the worker completes, fetch and review its commits:

```bash
# Fetch worker's commits
git fetch worker

# Check what commits were made (if empty, worker may not have committed!)
git log --oneline HEAD..worker/main

# Review the actual changes
git diff HEAD..worker/main

# Review individual commit messages and content
git log -p HEAD..worker/main
```

**If worker made changes but didn't commit:** Use `opencode run --attach --continue` to ask it to commit.

### Step 3: Validate Before Accepting

**Only accept commits that represent valid incremental progress:**

- Does the commit actually address the task?
- Is the code correct and complete?
- Are commit messages clear and descriptive?
- Are there any obvious bugs or issues?

**If the work is NOT acceptable**, send feedback and iterate:

```bash
opencode run --attach "$OPENCODE_WORKER_URL" --format json --continue "The commit needs changes: <specific feedback>. Please fix and commit again."
```

Then repeat Step 2 after worker makes corrections.

### Step 4: Merge Validated Commits

Only after review confirms the commits are good:

```bash
# Merge all worker commits
git merge worker/main --no-edit

# Or cherry-pick specific commits if only some are good
git cherry-pick <commit-sha>
```

### Step 5: Create PR When Complete

After all subtasks are done and merged, create the PR using service-gator.

### What You Must NOT Do

- **DO NOT** write code or make file changes yourself
- **DO NOT** skip the delegation step
- **DO NOT** merge without reviewing commits first
- **DO NOT** accept commits that don't represent real progress
- **DO NOT** create a PR before delegating and reviewing work

### Troubleshooting

**Worker not responding:** Check health with `curl -s $OPENCODE_WORKER_URL/health`
**No commits after worker done:** Worker may have forgotten to commit; send reminder with `--continue`
**Need to start fresh:** Omit `--continue` to start a new session

### Example Workflow

```bash
# 1. DELEGATE: Send task to worker (blocks until complete)
opencode run --attach "$OPENCODE_WORKER_URL" --format json "Add a LICENSE section to README.md explaining this project uses Apache 2.0. Commit when done."

# 2. FETCH: Get worker's commits
git fetch worker

# 3. REVIEW: Check what was done
git log --oneline HEAD..worker/main
git diff HEAD..worker/main

# 4. VALIDATE: If changes need work, send feedback
opencode run --attach "$OPENCODE_WORKER_URL" --format json --continue "Please also add a copyright year. Commit again."
git fetch worker

# 5. MERGE: Accept the validated work
git merge worker/main --no-edit

# 6. PR: Create pull request via service-gator
```
"#,
        worker_port = WORKER_OPENCODE_PORT
    )
}

/// Generate the complete system prompt for an agent, optionally including
/// orchestration instructions.
///
/// # Arguments
///
/// * `task` - The user's task description
/// * `enable_gator` - Whether service-gator is enabled for forge operations
/// * `enable_orchestration` - Whether multi-agent orchestration is enabled
///
/// # Returns
///
/// A formatted markdown string containing the complete system prompt
pub fn generate_system_prompt(
    task: &str,
    enable_gator: bool,
    enable_orchestration: bool,
) -> String {
    // Build service-gator instructions if enabled
    let gator_instructions = if enable_gator {
        r#"
## IMPORTANT: GitHub/GitLab Operations

For GitHub/GitLab operations (PRs, issues, etc.), use the **service-gator** MCP tool.
The `gh` and `glab` CLI tools are NOT available in this environment.
"#
        .to_string()
    } else {
        String::new()
    };

    // Add orchestration instructions for task owner if enabled
    let orchestration_section = if enable_orchestration {
        orchestration_instructions()
    } else {
        String::new()
    };

    format!(
        r#"# devaipod Task

You are running as an AI agent in a **devaipod** sandboxed environment.

## Your Task

{task}

## Guidelines

1. Work on the task described above
2. Make commits with clear, descriptive messages
3. When done, summarize what you accomplished
{gator_instructions}{orchestration_section}
"#,
        task = task,
        gator_instructions = gator_instructions,
        orchestration_section = orchestration_section
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orchestration_instructions_contains_worker_port() {
        let instructions = orchestration_instructions();
        assert!(
            instructions.contains(&format!("localhost:{}", WORKER_OPENCODE_PORT)),
            "Should contain worker port"
        );
    }

    #[test]
    fn test_orchestration_instructions_contains_git_commands() {
        let instructions = orchestration_instructions();
        assert!(
            instructions.contains("git fetch worker"),
            "Should explain how to fetch worker commits"
        );
        assert!(
            instructions.contains("git merge worker/main"),
            "Should explain how to merge worker commits"
        );
    }

    #[test]
    fn test_orchestration_instructions_uses_opencode_cli() {
        let instructions = orchestration_instructions();
        assert!(
            instructions.contains("opencode run --attach"),
            "Should use opencode run --attach for delegation"
        );
        assert!(
            instructions.contains("OPENCODE_WORKER_URL"),
            "Should reference OPENCODE_WORKER_URL env var"
        );
        assert!(
            instructions.contains("--format json"),
            "Should use --format json for machine-readable output"
        );
        assert!(
            instructions.contains("--continue"),
            "Should explain --continue for follow-up messages"
        );
    }

    #[test]
    fn test_orchestration_instructions_requires_delegation() {
        let instructions = orchestration_instructions();
        assert!(
            instructions.contains("MUST"),
            "Should use MUST to indicate mandatory steps"
        );
        assert!(
            instructions.contains("DO NOT"),
            "Should have clear prohibitions"
        );
        assert!(
            instructions.contains("Delegate to Worker (REQUIRED)"),
            "Should mark delegation as required"
        );
        assert!(
            instructions.contains("Fetch and Review Commits (REQUIRED)"),
            "Should mark review as required"
        );
    }

    #[test]
    fn test_orchestration_instructions_has_troubleshooting() {
        let instructions = orchestration_instructions();
        assert!(
            instructions.contains("Troubleshooting"),
            "Should have troubleshooting section"
        );
        assert!(
            instructions.contains("Worker not responding"),
            "Should address worker not responding scenario"
        );
        assert!(
            instructions.contains("/health"),
            "Should include health check endpoint"
        );
    }

    #[test]
    fn test_generate_system_prompt_basic() {
        let prompt = generate_system_prompt("Fix the bug", false, false);
        assert!(prompt.contains("Fix the bug"), "Should contain task");
        assert!(prompt.contains("# devaipod Task"), "Should have header");
        assert!(
            !prompt.contains("service-gator"),
            "Should not mention gator when disabled"
        );
        assert!(
            !prompt.contains("Multi-Agent Orchestration"),
            "Should not mention orchestration when disabled"
        );
    }

    #[test]
    fn test_generate_system_prompt_with_gator() {
        let prompt = generate_system_prompt("Fix the bug", true, false);
        assert!(
            prompt.contains("service-gator"),
            "Should mention service-gator when enabled"
        );
        assert!(
            !prompt.contains("Multi-Agent Orchestration"),
            "Should not mention orchestration when disabled"
        );
    }

    #[test]
    fn test_generate_system_prompt_with_orchestration() {
        let prompt = generate_system_prompt("Implement feature", false, true);
        assert!(
            prompt.contains("Multi-Agent Orchestration"),
            "Should include orchestration section"
        );
        assert!(
            prompt.contains("worker agent"),
            "Should mention worker agent"
        );
        assert!(
            prompt.contains("git fetch worker"),
            "Should include git commands"
        );
    }

    #[test]
    fn test_generate_system_prompt_with_both() {
        let prompt = generate_system_prompt("Complex task", true, true);
        assert!(
            prompt.contains("service-gator"),
            "Should mention service-gator"
        );
        assert!(
            prompt.contains("Multi-Agent Orchestration"),
            "Should include orchestration section"
        );
    }
}
