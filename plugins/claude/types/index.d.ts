/** The session whose start the plugin last saw, and its transcript's path. */
export type AiTranscript = { session: string; path: string }
/** What `/fold` compacts a session attached to no agent to: `ai view`, then the session's handoff; empty once used. */
export type AiCompaction = { view: string; handoff: string }
/** Recall forks: the ids of those known, and the prompts of those started but not yet known by id. */
export type AiRecall = { forks: string[]; prompts: string[] }

declare module 'claude-code' {
  interface PluginState {
    ai: { transcript: AiTranscript; compaction: AiCompaction; recall: AiRecall }
  }
}
