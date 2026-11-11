/** The session whose start the plugin last saw, and its transcript's path. */
export type AiTranscript = { session: string; path: string }
export type AiView = string
/** Recall forks: the ids of those known, and the prompts of those started but not yet known by id. */
export type AiRecall = { forks: string[]; prompts: string[] }

declare module 'claude-code' {
  interface PluginState {
    ai: { transcript: AiTranscript; view: AiView; recall: AiRecall }
  }
}
