# Worker Orchestration API

Now that multi-agent container orchestration is implemented (task-owner + worker containers), the next step is providing a programmatic API for the task owner to assign and review worker subtasks.

## Options

### MCP tools via service-gator

Extend service-gator with orchestration tools:

- `assign_worker`: Start a subtask on the worker
- `worker_status`: Check if worker is idle/running/completed
- `fetch_worker`: Get summary of worker's commits
- `review_worker`: Merge, reject, or request iteration

### OpenCode skill

Alternatively, an OpenCode skill could wrap git operations to provide orchestration semantics without new MCP tools. The skill would guide the agent through:

1. Writing a task description to a file the worker monitors
2. Polling for worker completion (via git status or file marker)
3. Fetching and reviewing worker commits with git
4. Merging or providing feedback

This approach uses existing primitives (files, git) rather than new tools.

## Open questions

- Should workers share the same LLM model as task owner, or be independently configured?
- If task owner crashes, can it resume with the worker's state?
- Should workers ever spawn sub-workers? (Initially: no)
- After merge/reject, should worker `git reset --hard` to owner's HEAD?
