export const RAW_EVENT_SCHEMA_VERSION = 1 as const

export type MousePayload = {
  x: number
  y: number
  button?: 'left' | 'right'
}

export type KeyPayload = {
  x?: number
  y?: number
  keyCode: number
  modifiers?: string[]
}

export type FrontmostAppPayload = {
  sessionId?: string
  bundleId?: string
  pid?: number
  name?: string
}

export type ScreenFramePayload = {
  sessionId?: string
  frameId: string
  path: string
}

export type AxSelector = {
  targetApp?: { bundleId: string }
  ax?: Record<string, unknown>
}

export type AxSnapshotPayload = {
  snapshotId: string
  x: number
  y: number
  bundleId?: string
  role?: string
  subrole?: string
  title?: string
  description?: string
  selector?: AxSelector
}

export type RecorderPayloadByType = {
  mouse_down: MousePayload
  mouse_up: MousePayload
  key_down: KeyPayload
  key_up: KeyPayload
  frontmost_app_changed: FrontmostAppPayload
  screen_frame: ScreenFramePayload
  ax_snapshot: AxSnapshotPayload
}

export type RecorderEventType = keyof RecorderPayloadByType

export type RawTraceEvent<TType extends RecorderEventType = RecorderEventType> = {
  schemaVersion: typeof RAW_EVENT_SCHEMA_VERSION
  sourceVersion?: number
  type: TType
  eventId?: string
  recordedAtMs: number
  payload: RecorderPayloadByType[TType]
}
