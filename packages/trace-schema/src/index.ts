export type RawTraceEvent =
  | { t: number; kind: 'mouseDown'; x: number; y: number; button: 'left' | 'right' }
  | { t: number; kind: 'mouseUp'; x: number; y: number; button: 'left' | 'right' }
  | { t: number; kind: 'keyDown'; keyCode: number; modifiers: string[] }
  | { t: number; kind: 'keyUp'; keyCode: number; modifiers: string[] }
  | { t: number; kind: 'frontmostAppChanged'; bundleId: string; pid: number }
  | { t: number; kind: 'screenFrame'; frameId: string; path: string }
  | { t: number; kind: 'axSnapshot'; snapshotId: string }
  | { t: number; kind: 'semanticAction'; actionId: string }

