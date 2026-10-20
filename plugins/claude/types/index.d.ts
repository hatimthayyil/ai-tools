export type AiTranscript = string
export type AiView = string
export type AiCompress = boolean

declare module 'claude-code' {
  interface PluginState {
    ai: { transcript: AiTranscript; view: AiView; compress: AiCompress }
  }
}
