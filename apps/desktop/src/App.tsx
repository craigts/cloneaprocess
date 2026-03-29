import { useEffect, useMemo, useState } from 'react'
import { invoke, isTauri } from '@tauri-apps/api/core'

type BackendStatus = {
  appVersion: string
  platform: string
  recordingsRoot: string
  databasePath: string
  startedAtMs: number
  sessionCount: number
  rawEventCount: number
  keyframeCount: number
  storageSchemaVersion: number
  workflowIrVersion: number
  recorderBinary: string
  recorderPermissions: Record<string, boolean>
  storageReady: boolean
  recordingsRootReady: boolean
  recorderBinaryExists: boolean
  helperHealth: 'ready' | 'missing_binary'
}

type RecorderStatus = {
  active: boolean
  sessionExternalId: string | null
  sessionRowId: number | null
  eventCount: number
  frameCount: number
  permissions: Record<string, boolean>
  recorderBinary: string
}

type SessionSummary = {
  id: number
  externalId: string
  label: string | null
  startedAtMs: number
  endedAtMs: number | null
  status: string
  appTransitionCount: number
  axSnapshotCount: number
  keyframeCount: number
  lastError: string | null
  createdAtMs: number
}

type TimelineEvent = {
  id: number
  sessionId: number
  sequence: number
  eventType: string
  eventJson: string
  recordedAtMs: number
  createdAtMs: number
}

type WorkflowDraft = {
  workflowJson: string
  stepCount: number
}

type ParsedEventPayload = {
  payload?: Record<string, unknown>
}

const browserFallbackStatus: BackendStatus = {
  appVersion: 'browser-preview',
  platform: 'browser',
  recordingsRoot: './recordings',
  databasePath: './storage/cloneaprocess.sqlite3',
  startedAtMs: 0,
  sessionCount: 0,
  rawEventCount: 0,
  keyframeCount: 0,
  storageSchemaVersion: 1,
  workflowIrVersion: 1,
  recorderBinary: './native/mac-recorder-service/.build/debug/RecorderService',
  recorderPermissions: {},
  storageReady: true,
  recordingsRootReady: true,
  recorderBinaryExists: true,
  helperHealth: 'ready',
}

export function App() {
  const [status, setStatus] = useState<BackendStatus | null>(null)
  const [recorder, setRecorder] = useState<RecorderStatus | null>(null)
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [selectedSessionId, setSelectedSessionId] = useState<number | null>(null)
  const [events, setEvents] = useState<TimelineEvent[]>([])
  const [workflowDraft, setWorkflowDraft] = useState<WorkflowDraft | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)
  const [timelineError, setTimelineError] = useState<string | null>(null)

  useEffect(() => {
    void refreshAll()
  }, [])

  useEffect(() => {
    if (selectedSessionId == null) {
      setEvents([])
      return
    }

    invoke<TimelineEvent[]>('list_session_events', { sessionId: selectedSessionId, limit: 250 })
      .then((response) => {
        setEvents(response)
        setTimelineError(null)
      })
      .catch((err) => {
        setEvents([])
        setTimelineError(String(err))
      })

    invoke<WorkflowDraft>('compile_workflow_preview', { sessionId: selectedSessionId })
      .then((response) => {
        setWorkflowDraft(response)
      })
      .catch(() => {
        setWorkflowDraft(null)
      })
  }, [selectedSessionId])

  async function refreshAll(preferredSessionId?: number | null) {
    try {
      const [systemStatus, recorderStatus, sessionRows] = await Promise.all([
        invoke<BackendStatus>('system_status'),
        invoke<RecorderStatus>('recorder_status'),
        invoke<SessionSummary[]>('list_sessions', { limit: 20 }),
      ])

      setStatus(systemStatus)
      setRecorder(recorderStatus)
      setSessions(sessionRows)
      setError(null)

      const nextSelection =
        preferredSessionId ??
        selectedSessionId ??
        recorderStatus.sessionRowId ??
        sessionRows[0]?.id ??
        null

      setSelectedSessionId(nextSelection)
    } catch (err) {
      setStatus(browserFallbackStatus)
      setRecorder(null)
      setSessions([])
      setSelectedSessionId(null)
      setEvents([])
      setWorkflowDraft(null)
      setError(String(err))
    }
  }

  async function handleRecorderAction(command: 'start_recording' | 'stop_recording') {
    try {
      const recorderStatus = await invoke<RecorderStatus>(command)
      setRecorder(recorderStatus)
      setActionError(null)
      await refreshAll(recorderStatus.sessionRowId)
    } catch (err) {
      setActionError(String(err))
    }
  }

  const selectedSession = useMemo(
    () => sessions.find((session) => session.id === selectedSessionId) ?? null,
    [selectedSessionId, sessions],
  )
  const prerequisites = useMemo(() => {
    if (!status) {
      return []
    }

    return [
      {
        id: 'storage',
        label: 'Storage ready',
        ready: status.storageReady && status.recordingsRootReady,
        detail: status.storageReady
          ? `Database at ${status.databasePath}`
          : 'App data directory is not writable yet.',
        remediation: 'Restart after granting app data access or resolving the app data path.',
      },
      {
        id: 'helper',
        label: 'Recorder helper',
        ready: status.recorderBinaryExists && status.helperHealth === 'ready',
        detail: status.recorderBinaryExists
          ? status.recorderBinary
          : `Missing helper binary at ${status.recorderBinary}`,
        remediation: 'Run `npm run desktop:run` to rebuild the Swift recorder helper.',
      },
      {
        id: 'accessibility',
        label: 'Accessibility',
        ready: Boolean(recorder?.permissions.accessibility ?? status.recorderPermissions.accessibility),
        detail: 'Required for event taps and AX snapshots.',
        remediation: 'Enable the app in System Settings > Privacy & Security > Accessibility.',
      },
      {
        id: 'screen-recording',
        label: 'Screen Recording',
        ready: Boolean(recorder?.permissions.screenRecording ?? status.recorderPermissions.screenRecording),
        detail: 'Required for keyframe capture.',
        remediation: 'Enable the app in System Settings > Privacy & Security > Screen Recording.',
      },
    ]
  }, [recorder, status])
  const canStartRecording = prerequisites.length > 0 && prerequisites.every((item) => item.ready)

  return (
    <main className="shell">
      <section className="hero">
        <p className="eyebrow">macOS-first automation workbench</p>
        <h1>Record first. Inspect what actually landed.</h1>
        <p className="lede">
          The app now surfaces recorder status, saved sessions, and the raw event timeline that
          is currently being persisted into SQLite.
        </p>
      </section>

      <section className="panel">
        <header className="panel-header">
          <div>
            <p className="panel-kicker">Core status</p>
            <h2>Rust bridge</h2>
          </div>
          <span className={`status-pill ${error ? 'status-pill--warning' : ''}`}>
            {error ? 'browser fallback' : 'tauri connected'}
          </span>
        </header>

        {status ? (
          <dl className="status-grid">
            <div>
              <dt>Version</dt>
              <dd>{status.appVersion}</dd>
            </div>
            <div>
              <dt>Platform</dt>
              <dd>{status.platform}</dd>
            </div>
            <div>
              <dt>Recordings root</dt>
              <dd>{status.recordingsRoot}</dd>
            </div>
            <div>
              <dt>Database path</dt>
              <dd>{status.databasePath}</dd>
            </div>
            <div>
              <dt>Sessions</dt>
              <dd>{status.sessionCount}</dd>
            </div>
            <div>
              <dt>Raw events</dt>
              <dd>{status.rawEventCount}</dd>
            </div>
            <div>
              <dt>Keyframes</dt>
              <dd>{status.keyframeCount}</dd>
            </div>
          </dl>
        ) : (
          <p className="loading">Connecting to Rust core...</p>
        )}

        {error ? <p className="note">{error}</p> : null}
      </section>

      <section className="panel">
        <header className="panel-header">
          <div>
            <p className="panel-kicker">Recorder bridge</p>
            <h2>Capture controls</h2>
          </div>
          <span className={`status-pill ${recorder?.active ? '' : 'status-pill--warning'}`}>
            {recorder?.active ? 'recording' : 'idle'}
          </span>
        </header>

        <div className="actions">
          <button
            type="button"
            disabled={!canStartRecording}
            onClick={() => void handleRecorderAction('start_recording')}
          >
            Start recording
          </button>
          <button type="button" onClick={() => void handleRecorderAction('stop_recording')}>
            Stop recording
          </button>
          <button type="button" onClick={() => void refreshAll(selectedSessionId)}>
            Refresh timeline
          </button>
        </div>

        {recorder ? (
          <>
            <dl className="status-grid">
              <div>
                <dt>Recorder binary</dt>
                <dd>{recorder.recorderBinary}</dd>
              </div>
              <div>
                <dt>Session</dt>
                <dd>{recorder.sessionExternalId ?? 'none'}</dd>
              </div>
              <div>
                <dt>Events ingested</dt>
                <dd>{recorder.eventCount}</dd>
              </div>
              <div>
                <dt>Frames ingested</dt>
                <dd>{recorder.frameCount}</dd>
              </div>
              <div>
                <dt>Accessibility</dt>
                <dd>{String(recorder.permissions.accessibility ?? false)}</dd>
              </div>
              <div>
                <dt>Screen recording</dt>
                <dd>{String(recorder.permissions.screenRecording ?? false)}</dd>
              </div>
            </dl>

            <div className="prereq-grid">
              {prerequisites.map((item) => (
                <article key={item.id} className={`prereq-card ${item.ready ? '' : 'prereq-card--blocked'}`}>
                  <div className="prereq-card__header">
                    <strong>{item.label}</strong>
                    <span className={`status-pill ${item.ready ? '' : 'status-pill--warning'}`}>
                      {item.ready ? 'ready' : 'action needed'}
                    </span>
                  </div>
                  <p>{item.detail}</p>
                  {!item.ready ? <p className="note prereq-card__note">{item.remediation}</p> : null}
                </article>
              ))}
            </div>
          </>
        ) : (
          <p className="loading">Recorder bridge status unavailable.</p>
        )}

        {actionError ? <p className="note">{actionError}</p> : null}
      </section>

      <section className="panel timeline-panel">
        <header className="panel-header">
          <div>
            <p className="panel-kicker">Recorded sessions</p>
            <h2>Timeline</h2>
          </div>
          <span className="status-pill">{sessions.length} loaded</span>
        </header>

        <div className="timeline-layout">
          <aside className="session-list">
            {sessions.length === 0 ? (
              <p className="loading">No sessions stored yet.</p>
            ) : (
              sessions.map((session) => (
                <button
                  key={session.id}
                  type="button"
                  className={`session-card ${session.id === selectedSessionId ? 'session-card--active' : ''}`}
                  onClick={() => setSelectedSessionId(session.id)}
                >
                  <span className="session-card__title">{session.label ?? session.externalId}</span>
                  <span className="session-card__meta">#{session.id}</span>
                  <span className="session-card__meta">{formatTimestamp(session.startedAtMs)}</span>
                  <span className="session-card__meta">{session.status}</span>
                  <span className="session-card__meta">
                    {session.appTransitionCount} app hops, {session.axSnapshotCount} AX, {session.keyframeCount} frames
                  </span>
                  {session.lastError ? <span className="session-card__meta">Last error: {session.lastError}</span> : null}
                </button>
              ))
            )}
          </aside>

          <div className="timeline-view">
            {selectedSession ? (
              <div className="timeline-summary">
                <strong>{selectedSession.label ?? selectedSession.externalId}</strong>
                <span>{formatTimestamp(selectedSession.startedAtMs)}</span>
                <span>{events.length} events loaded</span>
                <span>{selectedSession.appTransitionCount} app switches</span>
                <span>{selectedSession.axSnapshotCount} AX snapshots</span>
                <span>{selectedSession.keyframeCount} keyframes</span>
              </div>
            ) : (
              <p className="loading">Select a session to inspect events.</p>
            )}

            {timelineError ? <p className="note">{timelineError}</p> : null}

            <div className="timeline-events">
              {events.map((event) => (
                <TimelineEventCard key={event.id} event={event} />
              ))}
            </div>
          </div>
        </div>
      </section>

      <section className="panel">
        <header className="panel-header">
          <div>
            <p className="panel-kicker">Workflow draft</p>
            <h2>Compiler v0</h2>
          </div>
          <span className="status-pill">{workflowDraft?.stepCount ?? 0} steps</span>
        </header>

        {workflowDraft ? (
          <pre className="timeline-event__payload workflow-preview">{workflowDraft.workflowJson}</pre>
        ) : (
          <p className="loading">Select a session with AX snapshots to generate a workflow draft.</p>
        )}
      </section>
    </main>
  )
}

function TimelineEventCard({ event }: { event: TimelineEvent }) {
  const parsed = parseEventJson(event.eventJson)
  const payload = parsed.payload ?? {}
  const framePath = typeof payload.path === 'string' ? payload.path : null
  const [imageSrc, setImageSrc] = useState<string | null>(null)

  useEffect(() => {
    let revokedUrl: string | null = null
    let cancelled = false

    if (!framePath) {
      setImageSrc(null)
      return
    }

    if (!isTauri()) {
      setImageSrc(framePath)
      return
    }

    invoke<number[]>('load_keyframe_bytes', { path: framePath })
      .then((bytes) => {
        if (cancelled) {
          return
        }

        const blob = new Blob([new Uint8Array(bytes)], { type: 'image/jpeg' })
        revokedUrl = URL.createObjectURL(blob)
        setImageSrc(revokedUrl)
      })
      .catch(() => {
        if (!cancelled) {
          setImageSrc(null)
        }
      })

    return () => {
      cancelled = true
      if (revokedUrl) {
        URL.revokeObjectURL(revokedUrl)
      }
    }
  }, [framePath])

  return (
    <article className="timeline-event">
      <div className="timeline-event__header">
        <strong>{event.eventType}</strong>
        <span>seq {event.sequence}</span>
        <span>{formatTimestamp(event.recordedAtMs)}</span>
      </div>

      {imageSrc ? (
        <img className="timeline-event__frame" src={imageSrc} alt={`Keyframe for ${event.eventType}`} />
      ) : null}

      <pre className="timeline-event__payload">{JSON.stringify(payload, null, 2)}</pre>
    </article>
  )
}

function parseEventJson(value: string): ParsedEventPayload {
  try {
    return JSON.parse(value) as ParsedEventPayload
  } catch {
    return {}
  }
}

function formatTimestamp(timestamp: number) {
  return new Date(timestamp).toLocaleString()
}
