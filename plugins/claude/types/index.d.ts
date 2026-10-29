export type AiTranscript = string
export type AiView = string
export type AiCompress = boolean
/** Recall forks: the ids of those known, and the prompts of those started but not yet known by id. */
export type AiRecall = { forks: string[]; prompts: string[] }

declare module 'claude-code' {
  interface PluginState {
    ai: { transcript: AiTranscript; view: AiView; compress: AiCompress; recall: AiRecall }
  }
}
