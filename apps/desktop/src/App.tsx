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
  recorderTransportMode: 'subprocess_bridge' | 'xpc_mach_service' | 'xpc_bundled_service'
  recorderTransportTarget: string
  recorderTransportReady: boolean
  recorderTransportError: string | null
  recorderProtocolVersion: number | null
  recorderProtocolMin: number | null
  recorderProtocolCapabilities: string[]
  recorderProtocolCompatible: boolean
  recorderPermissions: Record<string, boolean>
  storageReady: boolean
  recordingsRootReady: boolean
  recorderBinaryExists: boolean
  helperHealth: 'ready' | 'missing_binary' | 'transport_unavailable' | 'protocol_mismatch'
}

type RecorderStatus = {
  active: boolean
  sessionExternalId: string | null
  sessionRowId: number | null
  eventCount: number
  frameCount: number
  permissions: Record<string, boolean>
  recorderBinary: string
  transportMode: 'subprocess_bridge' | 'xpc_mach_service' | 'xpc_bundled_service'
  transportTarget: string
  transportReady: boolean
  transportError: string | null
  protocolVersion: number | null
  protocolMin: number | null
  protocolCapabilities: string[]
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

type WorkflowExecution = {
  runRowId: number
  runExternalId: string
  workflowId: string
  workflowName: string
  status: string
  stepCount: number
  completedStepCount: number
  failedStepIndex: number | null
  lastError: string | null
}

type WorkflowRun = {
  id: number
  externalId: string
  workflowId: string
  workflowName: string
  sourceSessionId: number | null
  status: string
  startedAtMs: number
  endedAtMs: number | null
  stepCount: number
  completedStepCount: number
  failedStepIndex: number | null
  lastError: string | null
  createdAtMs: number
}

type WorkflowRunLog = {
  id: number
  workflowRunId: number
  sequence: number
  stepIndex: number | null
  eventType: string
  payloadJson: string
  recordedAtMs: number
  createdAtMs: number
}

type RetentionPolicy = {
  maxCompletedSessions: number
  maxSessionAgeDays: number
  orphanGraceHours: number
}

type RetentionCleanupResult = {
  policy: RetentionPolicy
  retainedSessionCount: number
  prunedSessionCount: number
  deletedKeyframeFileCount: number
  deletedSessionDirectoryCount: number
  deletedOrphanDirectoryCount: number
}

type ApprovalRequestPayload = {
  step_index?: number
  category?: string
  keyword?: string
  summary?: string
  detail?: string
}

type ParsedEventPayload = {
  schemaVersion?: number
  sourceVersion?: number
  type?: string
  eventId?: string
  recordedAtMs?: number
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
  recorderTransportMode: 'subprocess_bridge',
  recorderTransportTarget: './native/mac-recorder-service/.build/debug/RecorderService',
  recorderTransportReady: true,
  recorderTransportError: null,
  recorderProtocolVersion: 1,
  recorderProtocolMin: 1,
  recorderProtocolCapabilities: ['event_stream', 'permissions', 'ax_snapshot', 'screen_frame', 'subprocess_bridge'],
  recorderProtocolCompatible: true,
  recorderPermissions: {},
  storageReady: true,
  recordingsRootReady: true,
  recorderBinaryExists: true,
  helperHealth: 'ready',
}

const browserFallbackRetentionPolicy: RetentionPolicy = {
  maxCompletedSessions: 25,
  maxSessionAgeDays: 14,
  orphanGraceHours: 24,
}

export function App() {
  const [status, setStatus] = useState<BackendStatus | null>(null)
  const [recorder, setRecorder] = useState<RecorderStatus | null>(null)
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [selectedSessionId, setSelectedSessionId] = useState<number | null>(null)
  const [events, setEvents] = useState<TimelineEvent[]>([])
  const [workflowDraft, setWorkflowDraft] = useState<WorkflowDraft | null>(null)
  const [workflowRuns, setWorkflowRuns] = useState<WorkflowRun[]>([])
  const [selectedWorkflowRunId, setSelectedWorkflowRunId] = useState<number | null>(null)
  const [workflowRunLogs, setWorkflowRunLogs] = useState<WorkflowRunLog[]>([])
  const [retentionPolicy, setRetentionPolicy] = useState<RetentionPolicy>(browserFallbackRetentionPolicy)
  const [retentionDraft, setRetentionDraft] = useState<RetentionPolicy>(browserFallbackRetentionPolicy)
  const [error, setError] = useState<string | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)
  const [timelineError, setTimelineError] = useState<string | null>(null)
  const [workflowRunError, setWorkflowRunError] = useState<string | null>(null)
  const [workflowActionError, setWorkflowActionError] = useState<string | null>(null)
  const [retentionError, setRetentionError] = useState<string | null>(null)
  const [retentionMessage, setRetentionMessage] = useState<string | null>(null)
  const [executingWorkflow, setExecutingWorkflow] = useState(false)
  const [approvalActionPending, setApprovalActionPending] = useState(false)
  const [retentionActionPending, setRetentionActionPending] = useState(false)

  useEffect(() => {
    void refreshAll()
  }, [])

  useEffect(() => {
    if (selectedSessionId == null) {
      setEvents([])
      setWorkflowDraft(null)
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

  useEffect(() => {
    if (selectedWorkflowRunId == null) {
      setWorkflowRunLogs([])
      return
    }

    invoke<WorkflowRunLog[]>('list_workflow_run_logs', { workflowRunId: selectedWorkflowRunId, limit: 250 })
      .then((response) => {
        setWorkflowRunLogs(response)
        setWorkflowRunError(null)
      })
      .catch((err) => {
        setWorkflowRunLogs([])
        setWorkflowRunError(String(err))
      })
  }, [selectedWorkflowRunId])

  async function refreshAll(preferredSessionId?: number | null, preferredWorkflowRunId?: number | null) {
    const [systemResult, recorderResult, sessionsResult, runsResult, retentionResult] = await Promise.allSettled([
      invoke<BackendStatus>('system_status'),
      invoke<RecorderStatus>('recorder_status'),
      invoke<SessionSummary[]>('list_sessions', { limit: 20 }),
      invoke<WorkflowRun[]>('list_workflow_runs', { limit: 20 }),
      invoke<RetentionPolicy>('get_retention_policy'),
    ])

    if (systemResult.status !== 'fulfilled') {
      setStatus(browserFallbackStatus)
      setRecorder(null)
      setSessions([])
      setWorkflowRuns([])
      setRetentionPolicy(browserFallbackRetentionPolicy)
      setRetentionDraft(browserFallbackRetentionPolicy)
      setSelectedSessionId(null)
      setSelectedWorkflowRunId(null)
      setEvents([])
      setWorkflowDraft(null)
      setWorkflowRunLogs([])
      setError(String(systemResult.reason))
      return
    }

    const systemStatus = systemResult.value
    const recorderStatus = recorderResult.status === 'fulfilled' ? recorderResult.value : null
    const sessionRows = sessionsResult.status === 'fulfilled' ? sessionsResult.value : []
    const runRows = runsResult.status === 'fulfilled' ? runsResult.value : []
    const policy =
      retentionResult.status === 'fulfilled' ? retentionResult.value : retentionPolicy

    setStatus(systemStatus)
    setRecorder(recorderStatus)
    setSessions(sessionRows)
    setWorkflowRuns(runRows)
    setRetentionPolicy(policy)
    setRetentionDraft(policy)

    const nextSessionSelection = pickExistingId(
      [preferredSessionId, selectedSessionId, recorderStatus?.sessionRowId],
      sessionRows.map((session) => session.id),
    )

    const nextRunSelection = pickExistingId(
      [preferredWorkflowRunId, selectedWorkflowRunId],
      runRows.map((run) => run.id),
    )

    setSelectedSessionId(nextSessionSelection)
    setSelectedWorkflowRunId(nextRunSelection)

    const refreshErrors = [
      recorderResult.status === 'rejected' ? `recorder_status: ${String(recorderResult.reason)}` : null,
      sessionsResult.status === 'rejected' ? `list_sessions: ${String(sessionsResult.reason)}` : null,
      runsResult.status === 'rejected' ? `list_workflow_runs: ${String(runsResult.reason)}` : null,
      retentionResult.status === 'rejected' ? `get_retention_policy: ${String(retentionResult.reason)}` : null,
    ].filter(Boolean)

    setError(refreshErrors.length > 0 ? refreshErrors.join(' | ') : null)
  }

  function updateRetentionDraft<K extends keyof RetentionPolicy>(key: K, value: string) {
    const parsed = Number.parseInt(value, 10)
    setRetentionDraft((current) => ({
      ...current,
      [key]: Number.isFinite(parsed) && parsed >= 0 ? parsed : 0,
    }))
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

  async function handleExecuteWorkflow() {
    if (selectedSessionId == null) {
      return
    }

    try {
      setExecutingWorkflow(true)
      const execution = await invoke<WorkflowExecution>('execute_session_workflow', { sessionId: selectedSessionId })
      setWorkflowActionError(null)
      await refreshAll(selectedSessionId, execution.runRowId)
    } catch (err) {
      setWorkflowActionError(String(err))
    } finally {
      setExecutingWorkflow(false)
    }
  }

  async function handleApprovalAction(command: 'approve_workflow_run' | 'reject_workflow_run') {
    if (selectedWorkflowRunId == null) {
      return
    }

    try {
      setApprovalActionPending(true)
      const execution = await invoke<WorkflowExecution>(command, { workflowRunId: selectedWorkflowRunId })
      setWorkflowActionError(null)
      await refreshAll(selectedSessionId, execution.runRowId)
    } catch (err) {
      setWorkflowActionError(String(err))
    } finally {
      setApprovalActionPending(false)
    }
  }

  async function handleSaveRetentionPolicy() {
    try {
      setRetentionActionPending(true)
      const policy = await invoke<RetentionPolicy>('update_retention_policy', retentionDraft)
      setRetentionPolicy(policy)
      setRetentionDraft(policy)
      setRetentionError(null)
      setRetentionMessage('Retention policy saved.')
    } catch (err) {
      setRetentionError(String(err))
    } finally {
      setRetentionActionPending(false)
    }
  }

  async function handleRunRetentionCleanup() {
    try {
      setRetentionActionPending(true)
      const result = await invoke<RetentionCleanupResult>('run_retention_cleanup_now')
      setRetentionPolicy(result.policy)
      setRetentionDraft(result.policy)
      setRetentionError(null)
      setRetentionMessage(
        `Cleanup pruned ${result.prunedSessionCount} sessions, deleted ${result.deletedKeyframeFileCount} keyframes, and removed ${result.deletedOrphanDirectoryCount} orphan directories.`,
      )
      await refreshAll(selectedSessionId, selectedWorkflowRunId)
    } catch (err) {
      setRetentionError(String(err))
    } finally {
      setRetentionActionPending(false)
    }
  }

  const selectedSession = useMemo(
    () => sessions.find((session) => session.id === selectedSessionId) ?? null,
    [selectedSessionId, sessions],
  )
  const selectedWorkflowRun = useMemo(
    () => workflowRuns.find((run) => run.id === selectedWorkflowRunId) ?? null,
    [selectedWorkflowRunId, workflowRuns],
  )
  const pendingApproval = useMemo(() => {
    if (selectedWorkflowRun?.status !== 'awaiting_approval') {
      return null
    }

    const approvalLog = [...workflowRunLogs]
      .reverse()
      .find((log) => log.eventType === 'approval_requested')

    if (!approvalLog) {
      return null
    }

    return parseJsonRecord(approvalLog.payloadJson) as ApprovalRequestPayload
  }, [selectedWorkflowRun, workflowRunLogs])
  const prerequisites = useMemo(() => {
    if (!status) {
      return []
    }

    return [
      {
        id: 'storage',
        label: 'Storage ready',
        blocking: true,
        ready: status.storageReady && status.recordingsRootReady,
        detail: status.storageReady
          ? `Database at ${status.databasePath}`
          : 'App data directory is not writable yet.',
        remediation: 'Restart after granting app data access or resolving the app data path.',
      },
      {
        id: 'helper',
        label: status.recorderTransportMode === 'subprocess_bridge' ? 'Recorder helper' : 'Recorder transport',
        blocking: true,
        ready: status.helperHealth === 'ready',
        detail:
          status.recorderTransportMode === 'subprocess_bridge'
            ? !status.recorderBinaryExists
              ? `Missing helper binary at ${status.recorderBinary}`
              : !status.recorderProtocolCompatible
                ? `Protocol mismatch: helper reports v${status.recorderProtocolVersion ?? 'unknown'} (min ${
                    status.recorderProtocolMin ?? 'unknown'
                  }) with capabilities ${status.recorderProtocolCapabilities.join(', ') || 'none'}.`
                : status.recorderBinary
            : status.recorderTransportError
              ? `Configured ${
                  status.recorderTransportMode === 'xpc_bundled_service' ? 'bundled' : 'mach'
                } XPC service ${status.recorderTransportTarget}; latest probe failed: ${status.recorderTransportError}`
              : `Configured ${
                  status.recorderTransportMode === 'xpc_bundled_service' ? 'bundled' : 'mach'
                } XPC service ${status.recorderTransportTarget}`,
        remediation:
          status.recorderTransportMode === 'subprocess_bridge'
            ? status.helperHealth === 'protocol_mismatch'
              ? 'Rebuild or update the recorder helper so it matches the desktop app protocol contract.'
              : 'Run `npm run desktop:run` to rebuild the Swift recorder helper.'
            : 'Switch back to the subprocess bridge or finish the direct XPC client implementation.',
      },
      {
        id: 'accessibility',
        label: 'Accessibility',
        blocking: true,
        ready: Boolean(recorder?.permissions.accessibility ?? status.recorderPermissions.accessibility),
        detail: 'Required for event taps and AX snapshots.',
        remediation: 'Enable the app in System Settings > Privacy & Security > Accessibility.',
      },
      {
        id: 'screen-recording',
        label: 'Screen Recording',
        blocking: false,
        ready: Boolean(recorder?.permissions.screenRecording ?? status.recorderPermissions.screenRecording),
        detail: 'Optional for keyframe capture; recording can still run without screenshots.',
        remediation: 'Enable the app in System Settings > Privacy & Security > Screen Recording to capture keyframes.',
      },
    ]
  }, [recorder, status])
  const canStartRecording =
    prerequisites.length > 0 && prerequisites.every((item) => !item.blocking || item.ready)

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
            <div>
              <dt>Recorder transport</dt>
              <dd>{status.recorderTransportMode}</dd>
            </div>
            <div>
              <dt>Transport target</dt>
              <dd>{status.recorderTransportTarget}</dd>
            </div>
            <div>
              <dt>Protocol</dt>
              <dd>
                {status.recorderProtocolVersion != null
                  ? `v${status.recorderProtocolVersion} (min ${status.recorderProtocolMin ?? 'n/a'})`
                  : 'unknown'}
              </dd>
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
            <p className="panel-kicker">Storage hygiene</p>
            <h2>Retention</h2>
          </div>
          <span className="status-pill">
            keep {retentionPolicy.maxCompletedSessions} sessions / {retentionPolicy.maxSessionAgeDays} days
          </span>
        </header>

        <div className="settings-grid">
          <label className="settings-field">
            <span>Max completed sessions</span>
            <input
              type="number"
              min={0}
              value={retentionDraft.maxCompletedSessions}
              onChange={(event) => updateRetentionDraft('maxCompletedSessions', event.target.value)}
            />
          </label>
          <label className="settings-field">
            <span>Max age in days</span>
            <input
              type="number"
              min={0}
              value={retentionDraft.maxSessionAgeDays}
              onChange={(event) => updateRetentionDraft('maxSessionAgeDays', event.target.value)}
            />
          </label>
          <label className="settings-field">
            <span>Orphan grace in hours</span>
            <input
              type="number"
              min={0}
              value={retentionDraft.orphanGraceHours}
              onChange={(event) => updateRetentionDraft('orphanGraceHours', event.target.value)}
            />
          </label>
        </div>

        <p className="note">
          Completed sessions older than the age limit or beyond the retained count are pruned. Directories under the
          recordings roots that no longer match a stored session are removed after the grace window.
        </p>

        <div className="actions">
          <button type="button" disabled={retentionActionPending} onClick={() => void handleSaveRetentionPolicy()}>
            {retentionActionPending ? 'Applying…' : 'Save retention policy'}
          </button>
          <button type="button" disabled={retentionActionPending} onClick={() => void handleRunRetentionCleanup()}>
            Run cleanup now
          </button>
        </div>

        {retentionMessage ? <p className="note">{retentionMessage}</p> : null}
        {retentionError ? <p className="note">{retentionError}</p> : null}
      </section>

      <section className="panel">
        <header className="panel-header">
          <div>
            <p className="panel-kicker">Recorder transport</p>
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
                <dt>Transport mode</dt>
                <dd>{recorder.transportMode}</dd>
              </div>
              <div>
                <dt>Transport target</dt>
                <dd>{recorder.transportTarget}</dd>
              </div>
              <div>
                <dt>Recorder binary</dt>
                <dd>{recorder.recorderBinary || 'n/a'}</dd>
              </div>
              <div>
                <dt>Protocol</dt>
                <dd>
                  {recorder.protocolVersion != null
                    ? `v${recorder.protocolVersion} (min ${recorder.protocolMin ?? 'n/a'})`
                    : 'unknown'}
                </dd>
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
              <div>
                <dt>Capabilities</dt>
                <dd>{recorder.protocolCapabilities.join(', ') || 'none'}</dd>
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
          <div className="panel-actions">
            <span className="status-pill">{workflowDraft?.stepCount ?? 0} steps</span>
            <button
              type="button"
              disabled={selectedSessionId == null || executingWorkflow}
              onClick={() => void handleExecuteWorkflow()}
            >
              {executingWorkflow ? 'Running workflow…' : 'Run selected workflow'}
            </button>
          </div>
        </header>

        {workflowDraft ? (
          <pre className="timeline-event__payload workflow-preview">{workflowDraft.workflowJson}</pre>
        ) : (
          <p className="loading">Select a session with AX snapshots to generate a workflow draft.</p>
        )}

        {workflowActionError ? <p className="note">{workflowActionError}</p> : null}
      </section>

      <section className="panel timeline-panel">
        <header className="panel-header">
          <div>
            <p className="panel-kicker">Workflow execution</p>
            <h2>Run history</h2>
          </div>
          <span className="status-pill">{workflowRuns.length} loaded</span>
        </header>

        <div className="timeline-layout">
          <aside className="session-list">
            {workflowRuns.length === 0 ? (
              <p className="loading">No workflow runs stored yet.</p>
            ) : (
              workflowRuns.map((run) => (
                <button
                  key={run.id}
                  type="button"
                  className={`session-card ${run.id === selectedWorkflowRunId ? 'session-card--active' : ''}`}
                  onClick={() => setSelectedWorkflowRunId(run.id)}
                >
                  <span className="session-card__title">{run.workflowName}</span>
                  <span className="session-card__meta">Run #{run.id}</span>
                  <span className="session-card__meta">{formatTimestamp(run.startedAtMs)}</span>
                  <span className="session-card__meta">
                    {run.status} · {run.completedStepCount}/{run.stepCount} steps
                  </span>
                  {run.sourceSessionId != null ? (
                    <span className="session-card__meta">Source session #{run.sourceSessionId}</span>
                  ) : null}
                  {run.failedStepIndex != null ? (
                    <span className="session-card__meta">Failed at step {run.failedStepIndex}</span>
                  ) : null}
                  {run.lastError ? <span className="session-card__meta">Last error: {run.lastError}</span> : null}
                </button>
              ))
            )}
          </aside>

          <div className="timeline-view">
            {selectedWorkflowRun ? (
              <>
                <div className="timeline-summary">
                  <strong>{selectedWorkflowRun.workflowName}</strong>
                  <span>{selectedWorkflowRun.status}</span>
                  <span>{selectedWorkflowRun.completedStepCount} / {selectedWorkflowRun.stepCount} steps</span>
                  <span>{formatTimestamp(selectedWorkflowRun.startedAtMs)}</span>
                  {selectedWorkflowRun.endedAtMs != null ? <span>Ended {formatTimestamp(selectedWorkflowRun.endedAtMs)}</span> : null}
                </div>

                {pendingApproval ? (
                  <article className="approval-card">
                    <div className="approval-card__header">
                      <strong>Approval required</strong>
                      <span className="status-pill status-pill--warning">awaiting decision</span>
                    </div>
                    <p>{pendingApproval.summary ?? selectedWorkflowRun.lastError ?? 'Risky step is paused.'}</p>
                    <p className="note">
                      {pendingApproval.detail ?? 'Review the pending step in the run log before deciding.'}
                    </p>
                    <div className="approval-card__meta">
                      {pendingApproval.category ? <span>Category: {pendingApproval.category}</span> : null}
                      {pendingApproval.keyword ? <span>Keyword: {pendingApproval.keyword}</span> : null}
                      {pendingApproval.step_index != null ? <span>Step: {pendingApproval.step_index}</span> : null}
                    </div>
                    <div className="actions">
                      <button
                        type="button"
                        disabled={approvalActionPending}
                        onClick={() => void handleApprovalAction('approve_workflow_run')}
                      >
                        {approvalActionPending ? 'Applying…' : 'Approve and continue'}
                      </button>
                      <button
                        type="button"
                        disabled={approvalActionPending}
                        onClick={() => void handleApprovalAction('reject_workflow_run')}
                      >
                        Reject run
                      </button>
                    </div>
                  </article>
                ) : null}
              </>
            ) : (
              <p className="loading">Run a workflow or select a stored run to inspect logs.</p>
            )}

            {workflowRunError ? <p className="note">{workflowRunError}</p> : null}

            <div className="timeline-events">
              {workflowRunLogs.map((log) => (
                <WorkflowRunLogCard key={log.id} log={log} />
              ))}
            </div>
          </div>
        </div>
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

function WorkflowRunLogCard({ log }: { log: WorkflowRunLog }) {
  const payload = parseJsonRecord(log.payloadJson)

  return (
    <article className="timeline-event workflow-log">
      <div className="timeline-event__header">
        <strong>{log.eventType}</strong>
        <span>seq {log.sequence}</span>
        {log.stepIndex != null ? <span>step {log.stepIndex}</span> : null}
        <span>{formatTimestamp(log.recordedAtMs)}</span>
      </div>

      <pre className="timeline-event__payload">{JSON.stringify(payload, null, 2)}</pre>
    </article>
  )
}

function parseJsonRecord(value: string): Record<string, unknown> {
  try {
    return JSON.parse(value) as Record<string, unknown>
  } catch {
    return {}
  }
}

function pickExistingId(candidates: Array<number | null | undefined>, availableIds: number[]) {
  for (const candidate of candidates) {
    if (candidate != null && availableIds.includes(candidate)) {
      return candidate
    }
  }

  return availableIds[0] ?? null
}

function formatTimestamp(timestamp: number) {
  return new Date(timestamp).toLocaleString()
}
