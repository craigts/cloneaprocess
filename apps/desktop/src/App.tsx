import { useEffect, useMemo, useState } from 'react'
import { convertFileSrc, invoke, isTauri } from '@tauri-apps/api/core'

type BackendStatus = {
  appVersion: string
  platform: string
  recordingsRoot: string
  databasePath: string
  startedAtMs: number
  sessionCount: number
  rawEventCount: number
  storageSchemaVersion: number
  workflowIrVersion: number
  recorderBinary: string
  recorderPermissions: Record<string, boolean>
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
  storageSchemaVersion: 1,
  workflowIrVersion: 1,
  recorderBinary: './native/mac-recorder-service/.build/debug/RecorderService',
  recorderPermissions: {},
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
          <button type="button" onClick={() => void handleRecorderAction('start_recording')}>
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
  const imageSrc =
    framePath && isTauri()
      ? convertFileSrc(framePath)
      : framePath

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
