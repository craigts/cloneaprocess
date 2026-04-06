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
  description: string | null
  startedAtMs: number
  endedAtMs: number | null
  status: string
  appTransitionCount: number
  axSnapshotCount: number
  keyframeCount: number
  lastError: string | null
  createdAtMs: number
}

type WorkflowDraft = { workflowJson: string; stepCount: number }
type AiCompileResponse = { workflowJson: string; stepCount: number; model: string; promptTokens: number | null; outputTokens: number | null }
type WorkflowExecution = { runRowId: number; runExternalId: string; workflowId: string; workflowName: string; status: string; stepCount: number; completedStepCount: number; failedStepIndex: number | null; lastError: string | null }
type WorkflowRun = { id: number; externalId: string; workflowId: string; workflowName: string; sourceSessionId: number | null; status: string; startedAtMs: number; endedAtMs: number | null; stepCount: number; completedStepCount: number; failedStepIndex: number | null; lastError: string | null; createdAtMs: number }
type WorkflowRunLog = { id: number; workflowRunId: number; sequence: number; stepIndex: number | null; eventType: string; payloadJson: string; recordedAtMs: number; createdAtMs: number }
type PermissionCheck = { storageReady: boolean; recordingsRootReady: boolean; helperReady: boolean; helperError: string | null; accessibility: boolean; screenRecording: boolean }
type RetentionPolicy = { maxCompletedSessions: number; maxSessionAgeDays: number; orphanGraceHours: number }
type ApprovalRequestPayload = { step_index?: number; category?: string; keyword?: string; summary?: string; detail?: string }

const browserFallbackStatus: BackendStatus = {
  appVersion: 'browser-preview', platform: 'browser', recordingsRoot: './recordings', databasePath: './storage/cloneaprocess.sqlite3',
  startedAtMs: 0, sessionCount: 0, rawEventCount: 0, keyframeCount: 0, storageSchemaVersion: 1, workflowIrVersion: 1,
  recorderBinary: '', recorderTransportMode: 'subprocess_bridge', recorderTransportTarget: '', recorderTransportReady: true,
  recorderTransportError: null, recorderProtocolVersion: 1, recorderProtocolMin: 1,
  recorderProtocolCapabilities: [], recorderProtocolCompatible: true, recorderPermissions: {},
  storageReady: true, recordingsRootReady: true, recorderBinaryExists: true, helperHealth: 'ready',
}

export function App() {
  const [status, setStatus] = useState<BackendStatus | null>(null)
  const [recorder, setRecorder] = useState<RecorderStatus | null>(null)
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [selectedSessionId, setSelectedSessionId] = useState<number | null>(null)
  const [workflowDraft, setWorkflowDraft] = useState<WorkflowDraft | null>(null)
  const [workflowRuns, setWorkflowRuns] = useState<WorkflowRun[]>([])
  const [selectedWorkflowRunId, setSelectedWorkflowRunId] = useState<number | null>(null)
  const [workflowRunLogs, setWorkflowRunLogs] = useState<WorkflowRunLog[]>([])
  const [retentionPolicy, setRetentionPolicy] = useState<RetentionPolicy>({ maxCompletedSessions: 25, maxSessionAgeDays: 14, orphanGraceHours: 24 })
  const [error, setError] = useState<string | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)
  const [workflowActionError, setWorkflowActionError] = useState<string | null>(null)
  const [executingWorkflow, setExecutingWorkflow] = useState(false)
  const [approvalActionPending, setApprovalActionPending] = useState(false)
  const [wizardDismissed, setWizardDismissed] = useState(false)
  const [descriptionDraft, setDescriptionDraft] = useState('')
  const [descriptionSaving, setDescriptionSaving] = useState(false)
  const [aiWorkflow, setAiWorkflow] = useState<AiCompileResponse | null>(null)
  const [aiCompiling, setAiCompiling] = useState(false)
  const [aiRefining, setAiRefining] = useState(false)
  const [aiError, setAiError] = useState<string | null>(null)
  const [aiMessage, setAiMessage] = useState<string | null>(null)
  const [apiKeyDisplay, setApiKeyDisplay] = useState('')
  const [apiKeyDraft, setApiKeyDraft] = useState('')
  const [apiKeySaving, setApiKeySaving] = useState(false)
  const [showAdvanced, setShowAdvanced] = useState(false)

  // --- data loading ---

  useEffect(() => {
    void refreshAll()
    if (isTauri()) {
      invoke<string>('get_ai_api_key').then(setApiKeyDisplay).catch(() => {})
    }
  }, [])

  useEffect(() => {
    if (selectedSessionId == null) { setWorkflowDraft(null); return }
    invoke<WorkflowDraft>('compile_workflow_preview', { sessionId: selectedSessionId })
      .then(setWorkflowDraft).catch(() => setWorkflowDraft(null))
  }, [selectedSessionId])

  useEffect(() => {
    if (selectedWorkflowRunId == null) { setWorkflowRunLogs([]); return }
    invoke<WorkflowRunLog[]>('list_workflow_run_logs', { workflowRunId: selectedWorkflowRunId, limit: 250 })
      .then(setWorkflowRunLogs).catch(() => setWorkflowRunLogs([]))
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
      setRecorder(null); setSessions([]); setWorkflowRuns([])
      setError(String(systemResult.reason))
      return
    }

    const sys = systemResult.value
    const rec = recorderResult.status === 'fulfilled' ? recorderResult.value : null
    const sess = sessionsResult.status === 'fulfilled' ? sessionsResult.value : []
    const runs = runsResult.status === 'fulfilled' ? runsResult.value : []
    const pol = retentionResult.status === 'fulfilled' ? retentionResult.value : retentionPolicy

    setStatus(sys); setRecorder(rec); setSessions(sess); setWorkflowRuns(runs); setRetentionPolicy(pol)
    setSelectedSessionId(pickExistingId([preferredSessionId, selectedSessionId, rec?.sessionRowId], sess.map((s) => s.id)))
    setSelectedWorkflowRunId(pickExistingId([preferredWorkflowRunId, selectedWorkflowRunId], runs.map((r) => r.id)))
    setError(null)
  }

  // --- selected session ---

  const selectedSession = useMemo(() => sessions.find((s) => s.id === selectedSessionId) ?? null, [selectedSessionId, sessions])

  useEffect(() => {
    setDescriptionDraft(selectedSession?.description ?? '')
    setAiWorkflow(null); setAiError(null); setAiMessage(null)
  }, [selectedSession?.id])

  const selectedWorkflowRun = useMemo(() => workflowRuns.find((r) => r.id === selectedWorkflowRunId) ?? null, [selectedWorkflowRunId, workflowRuns])
  const pendingApproval = useMemo(() => {
    if (selectedWorkflowRun?.status !== 'awaiting_approval') return null
    const log = [...workflowRunLogs].reverse().find((l) => l.eventType === 'approval_requested')
    if (!log) return null
    return parseJson(log.payloadJson) as ApprovalRequestPayload
  }, [selectedWorkflowRun, workflowRunLogs])

  // --- setup wizard ---

  const setupSteps = useMemo(() => {
    if (!status) return []
    return [
      { id: 'storage', label: 'Storage', blocking: true, ready: status.storageReady && status.recordingsRootReady, description: 'The app needs a writable data directory.', remediation: 'Restart after granting app data access.', settingsPane: null as string | null },
      { id: 'helper', label: 'Recorder service', blocking: true, ready: status.helperHealth === 'ready', description: 'The native recorder service must be running.', remediation: 'Run `npm run desktop:run` to start it.', settingsPane: null as string | null },
      { id: 'accessibility', label: 'Accessibility', blocking: true, ready: Boolean(recorder?.permissions.accessibility ?? status.recorderPermissions.accessibility), description: 'Required to capture your clicks and keystrokes. Click "Open System Settings" and enable RecorderService.', remediation: 'Click + and browse to apps/desktop/src-tauri/resources/macos/RecorderService if not listed.', settingsPane: 'accessibility' },
      { id: 'screen-recording', label: 'Screen Recording', blocking: false, ready: Boolean(recorder?.permissions.screenRecording ?? status.recorderPermissions.screenRecording), description: 'Enables screenshots during recording. Optional but recommended.', remediation: 'Click + and add RecorderService from the same path.', settingsPane: 'screen_recording' },
    ]
  }, [recorder, status])

  const allBlockingReady = setupSteps.length > 0 && setupSteps.every((s) => !s.blocking || s.ready)
  const allReady = setupSteps.length > 0 && setupSteps.every((s) => s.ready)
  const currentStepIndex = setupSteps.findIndex((s) => !s.ready)
  const currentStep = currentStepIndex >= 0 ? setupSteps[currentStepIndex] : null
  const showWizard = !allReady && !wizardDismissed
  const canStartRecording = allBlockingReady
  const isRecording = recorder?.active ?? false

  // permission polling
  useEffect(() => {
    if (!showWizard || !isTauri()) return
    let pendingRestart = false
    const interval = setInterval(() => {
      invoke<PermissionCheck>('check_permissions').then((check) => {
        setStatus((prev) => prev ? { ...prev, storageReady: check.storageReady, recordingsRootReady: check.recordingsRootReady, recorderPermissions: { ...prev.recorderPermissions, accessibility: check.accessibility, screenRecording: check.screenRecording } } : prev)
        setRecorder((prev) => prev ? { ...prev, transportReady: check.helperReady, transportError: check.helperError, permissions: { ...prev.permissions, accessibility: check.accessibility, screenRecording: check.screenRecording } } : prev)
        if (!pendingRestart && (!check.accessibility || !check.screenRecording)) {
          pendingRestart = true
          invoke('restart_recorder_service').catch(() => {}).finally(() => { pendingRestart = false })
        }
      }).catch(() => {})
    }, 2000)
    return () => clearInterval(interval)
  }, [showWizard])

  // --- handlers ---

  function handleOpenSettings(pane: string) { invoke('open_system_settings_pane', { pane }).catch(() => {}) }

  async function handleRecord() {
    try {
      const command = isRecording ? 'stop_recording' : 'start_recording'
      const rec = await invoke<RecorderStatus>(command)
      setRecorder(rec); setActionError(null)
      await refreshAll(rec.sessionRowId)
    } catch (err) { setActionError(String(err)) }
  }

  async function handleSaveDescription() {
    if (selectedSessionId == null) return
    setDescriptionSaving(true)
    try {
      await invoke('update_session_description', { sessionId: selectedSessionId, description: descriptionDraft.trim() || null })
      await refreshAll(selectedSessionId, selectedWorkflowRunId)
    } catch (err) { setActionError(String(err)) }
    finally { setDescriptionSaving(false) }
  }

  async function handleAiCompile() {
    if (selectedSessionId == null) return
    setAiCompiling(true); setAiError(null); setAiMessage(null); setAiWorkflow(null)
    try {
      setAiWorkflow(await invoke<AiCompileResponse>('ai_compile_workflow', { sessionId: selectedSessionId }))
    } catch (err) { setAiError(String(err)) }
    finally { setAiCompiling(false) }
  }

  async function handleAiRefine() {
    if (!aiWorkflow || !latestRun) return
    const oldStepCount = aiWorkflow.stepCount
    setAiRefining(true); setAiError(null); setAiMessage(null)
    try {
      const result = await invoke<AiCompileResponse>('ai_refine_workflow', {
        workflowJson: aiWorkflow.workflowJson,
        workflowRunId: latestRun.id,
        sessionDescription: selectedSession?.description ?? null,
      })
      setAiWorkflow(result)
      const changed = result.workflowJson !== aiWorkflow.workflowJson
      setAiMessage(changed
        ? `AI updated the workflow (${oldStepCount} → ${result.stepCount} steps). Hit "Run it" to try the fix.`
        : 'AI returned the same workflow — it may not know how to fix this. Try editing your description with more detail.')
    } catch (err) { setAiError(String(err)) }
    finally { setAiRefining(false) }
  }

  async function handleRunWorkflow() {
    if (selectedSessionId == null) return
    setExecutingWorkflow(true)
    try {
      const exec = aiWorkflow
        ? await invoke<WorkflowExecution>('execute_workflow_json', { workflowJson: aiWorkflow.workflowJson, sourceSessionId: selectedSessionId })
        : await invoke<WorkflowExecution>('execute_session_workflow', { sessionId: selectedSessionId })
      setWorkflowActionError(null)
      await refreshAll(selectedSessionId, exec.runRowId)
    } catch (err) { setWorkflowActionError(String(err)) }
    finally { setExecutingWorkflow(false) }
  }

  async function handleApproval(command: 'approve_workflow_run' | 'reject_workflow_run') {
    if (selectedWorkflowRunId == null) return
    setApprovalActionPending(true)
    try {
      const exec = await invoke<WorkflowExecution>(command, { workflowRunId: selectedWorkflowRunId })
      await refreshAll(selectedSessionId, exec.runRowId)
    } catch (err) { setWorkflowActionError(String(err)) }
    finally { setApprovalActionPending(false) }
  }

  async function handleCopyLogs() {
    const run = latestRun
    if (!run) return

    // fetch full logs for this run
    let logs: WorkflowRunLog[] = []
    try {
      logs = await invoke<WorkflowRunLog[]>('list_workflow_run_logs', { workflowRunId: run.id, limit: 250 })
    } catch {}

    const sections: string[] = []

    sections.push(`## Run: ${run.workflowName}`)
    sections.push(`Status: ${run.status}`)
    sections.push(`Steps: ${run.completedStepCount}/${run.stepCount}`)
    if (run.failedStepIndex != null) sections.push(`Failed at step: ${run.failedStepIndex}`)
    if (run.lastError) sections.push(`Error: ${run.lastError}`)
    sections.push(`Started: ${formatTimestamp(run.startedAtMs)}`)
    if (run.endedAtMs) sections.push(`Ended: ${formatTimestamp(run.endedAtMs)}`)

    if (selectedSession?.description) {
      sections.push(`\n## Session description\n${selectedSession.description}`)
    }

    if (aiWorkflow) {
      sections.push(`\n## AI workflow (${aiWorkflow.stepCount} steps)\n\`\`\`json\n${aiWorkflow.workflowJson}\n\`\`\``)
    }

    if (logs.length > 0) {
      sections.push(`\n## Run logs`)
      for (const log of logs) {
        const payload = JSON.stringify(parseJson(log.payloadJson), null, 2)
        sections.push(`${log.eventType}${log.stepIndex != null ? ` (step ${log.stepIndex})` : ''} — ${formatTimestamp(log.recordedAtMs)}\n\`\`\`json\n${payload}\n\`\`\``)
      }
    }

    const text = sections.join('\n')
    try {
      await invoke('copy_to_clipboard', { text })
      setWorkflowActionError('Logs copied to clipboard.')
      setTimeout(() => setWorkflowActionError(null), 3000)
    } catch {
      // fallback: try browser API
      try {
        await navigator.clipboard.writeText(text)
        setWorkflowActionError('Logs copied to clipboard.')
        setTimeout(() => setWorkflowActionError(null), 3000)
      } catch {
        setWorkflowActionError('Failed to copy — check clipboard permissions.')
      }
    }
  }

  async function handleSaveApiKey() {
    setApiKeySaving(true)
    try {
      await invoke('set_ai_api_key', { apiKey: apiKeyDraft })
      setApiKeyDisplay(await invoke<string>('get_ai_api_key'))
      setApiKeyDraft('')
    } catch (err) { setAiError(String(err)) }
    finally { setApiKeySaving(false) }
  }

  // --- latest run for selected session ---
  const latestRun = useMemo(() => {
    if (selectedSessionId == null) return null
    return workflowRuns.find((r) => r.sourceSessionId === selectedSessionId) ?? null
  }, [selectedSessionId, workflowRuns])

  // ===================== RENDER =====================

  return (
    <main className="shell">

      {/* ---- SETUP WIZARD (only when needed) ---- */}
      {showWizard ? (
        <section className="panel wizard-panel">
          <h2>Welcome! Let's get set up.</h2>
          <div className="wizard-progress">
            {setupSteps.map((step, i) => (
              <div key={step.id} className={`wizard-pip ${step.ready ? 'wizard-pip--done' : i === currentStepIndex ? 'wizard-pip--active' : ''}`}>
                <span className="wizard-pip__dot" />
                <span className="wizard-pip__label">{step.label}</span>
              </div>
            ))}
          </div>
          {currentStep ? (
            <article className="wizard-step">
              <h3>{currentStep.label}{!currentStep.blocking ? <span className="wizard-optional">optional</span> : null}</h3>
              <p>{currentStep.description}</p>
              <div className="wizard-step__actions">
                {currentStep.settingsPane ? <button type="button" onClick={() => handleOpenSettings(currentStep.settingsPane!)}>Open System Settings</button> : null}
                {!currentStep.blocking ? <button type="button" className="wizard-skip" onClick={() => setWizardDismissed(true)}>Skip</button> : null}
              </div>
              <p className="note">{currentStep.remediation}</p>
            </article>
          ) : null}
        </section>
      ) : null}

      {/* ---- STEP 1: RECORD ---- */}
      <section className="panel">
        <div className="step-header">
          <span className="step-number">1</span>
          <div>
            <h2>Record what you do</h2>
            <p className="step-subtitle">Click record, do the thing you want to automate, then stop.</p>
          </div>
        </div>
        <div className="record-controls">
          <button
            type="button"
            className={`record-btn ${isRecording ? 'record-btn--active' : ''}`}
            disabled={!canStartRecording}
            onClick={() => void handleRecord()}
          >
            {isRecording ? 'Stop recording' : 'Start recording'}
          </button>
          {isRecording ? <span className="recording-indicator">Recording... {recorder?.eventCount ?? 0} events captured</span> : null}
        </div>
        {actionError ? <p className="note">{actionError}</p> : null}
      </section>

      {/* ---- STEP 2: DESCRIBE ---- */}
      <section className="panel">
        <div className="step-header">
          <span className="step-number">2</span>
          <div>
            <h2>Describe what you did</h2>
            <p className="step-subtitle">Pick a recording and tell the AI what you want to automate.</p>
          </div>
        </div>

        {sessions.length > 0 ? (
          <div className="recording-picker">
            {sessions.map((session) => (
              <button
                key={session.id}
                type="button"
                className={`recording-chip ${session.id === selectedSessionId ? 'recording-chip--active' : ''}`}
                onClick={() => setSelectedSessionId(session.id)}
              >
                <strong>{session.description || session.label || 'Untitled'}</strong>
                <span>{formatTimestamp(session.startedAtMs)}</span>
              </button>
            ))}
          </div>
        ) : (
          <p className="empty-state">No recordings yet. Hit record above to get started.</p>
        )}

        {selectedSession ? (
          <div className="description-area">
            <textarea
              rows={3}
              placeholder="What were you doing? What do you want to automate? (e.g., 'Open Chrome and go to Hacker News')"
              value={descriptionDraft}
              onChange={(e) => setDescriptionDraft(e.target.value)}
            />
            <button
              type="button"
              disabled={descriptionSaving || descriptionDraft === (selectedSession.description ?? '')}
              onClick={() => void handleSaveDescription()}
            >
              {descriptionSaving ? 'Saving...' : 'Save'}
            </button>
          </div>
        ) : null}
      </section>

      {/* ---- STEP 3: AUTOMATE ---- */}
      <section className="panel">
        <div className="step-header">
          <span className="step-number">3</span>
          <div>
            <h2>Create and run your automation</h2>
            <p className="step-subtitle">AI turns your recording into a reusable workflow.</p>
          </div>
        </div>

        {!apiKeyDisplay ? (
          <div className="api-key-prompt">
            <p>To use AI compilation, enter your Anthropic API key.</p>
            <div className="api-key-row">
              <input type="password" placeholder="sk-ant-..." value={apiKeyDraft} onChange={(e) => setApiKeyDraft(e.target.value)} />
              <button type="button" disabled={!apiKeyDraft.trim() || apiKeySaving} onClick={() => void handleSaveApiKey()}>
                {apiKeySaving ? 'Saving...' : 'Save key'}
              </button>
            </div>
          </div>
        ) : (
          <div className="automate-actions">
            <button type="button" className="primary-btn" disabled={selectedSessionId == null || aiCompiling} onClick={() => void handleAiCompile()}>
              {aiCompiling ? 'AI is thinking...' : 'Build automation'}
            </button>
            {aiWorkflow ? (
              <button type="button" className="primary-btn" disabled={executingWorkflow} onClick={() => void handleRunWorkflow()}>
                {executingWorkflow ? 'Running...' : 'Run it'}
              </button>
            ) : null}
          </div>
        )}

        {aiCompiling ? <p className="loading">Analyzing your recording and building steps...</p> : null}
        {aiRefining ? <p className="loading">AI is analyzing the failure and fixing the workflow...</p> : null}
        {aiMessage ? <p className="ai-message">{aiMessage}</p> : null}

        {aiWorkflow ? (
          <div className="workflow-result">
            <div className="workflow-result__header">
              <span className="status-pill">{aiWorkflow.stepCount} steps</span>
              {latestRun ? (
                <span className={`status-pill ${latestRun.status === 'completed' ? 'status-pill--success' : latestRun.status === 'failed' ? 'status-pill--warning' : ''}`}>
                  {latestRun.status === 'completed' ? 'Last run succeeded' : latestRun.status === 'failed' ? `Failed at step ${(latestRun.failedStepIndex ?? 0) + 1}` : latestRun.status}
                </span>
              ) : null}
            </div>
            <div className="workflow-steps">
              {(() => {
                try {
                  const wf = JSON.parse(aiWorkflow.workflowJson)
                  return (wf.steps ?? []).map((step: { kind: string; description?: string }, i: number) => (
                    <div key={i} className="workflow-step-card">
                      <span className="workflow-step-card__num">{i + 1}</span>
                      <span>{step.description || step.kind}</span>
                    </div>
                  ))
                } catch { return <pre className="code-block">{aiWorkflow.workflowJson}</pre> }
              })()}
            </div>
          </div>
        ) : null}

        {latestRun && latestRun.status === 'failed' ? (
          <div className="failure-actions">
            <button type="button" className="primary-btn" disabled={aiRefining || !aiWorkflow} onClick={() => void handleAiRefine()}>
              {aiRefining ? 'AI is fixing...' : 'Fix with AI'}
            </button>
            <button type="button" className="copy-logs-btn" onClick={() => void handleCopyLogs()}>
              Copy logs
            </button>
          </div>
        ) : null}

        {aiError ? <p className="note">{aiError}</p> : null}
        {workflowActionError ? <p className="note">{workflowActionError}</p> : null}

        {/* Approval gate */}
        {pendingApproval ? (
          <article className="approval-card">
            <strong>Approval needed</strong>
            <p>{pendingApproval.summary ?? 'A step needs your approval before continuing.'}</p>
            <div className="automate-actions">
              <button type="button" disabled={approvalActionPending} onClick={() => void handleApproval('approve_workflow_run')}>Approve</button>
              <button type="button" className="wizard-skip" disabled={approvalActionPending} onClick={() => void handleApproval('reject_workflow_run')}>Reject</button>
            </div>
          </article>
        ) : null}
      </section>

      {/* ---- ADVANCED (collapsed by default) ---- */}
      <details className="advanced-section" open={showAdvanced} onToggle={(e) => setShowAdvanced((e.target as HTMLDetailsElement).open)}>
        <summary className="advanced-toggle">Advanced details</summary>

        <section className="panel">
          <h3>API key</h3>
          <div className="api-key-row">
            <span className="api-key-display">{apiKeyDisplay || 'not set'}</span>
            <input type="password" placeholder="sk-ant-..." value={apiKeyDraft} onChange={(e) => setApiKeyDraft(e.target.value)} />
            <button type="button" disabled={!apiKeyDraft.trim() || apiKeySaving} onClick={() => void handleSaveApiKey()}>Save</button>
          </div>
        </section>

        {workflowDraft ? (
          <section className="panel">
            <h3>V1 compiler output ({workflowDraft.stepCount} steps)</h3>
            <pre className="code-block">{workflowDraft.workflowJson}</pre>
          </section>
        ) : null}

        {aiWorkflow ? (
          <section className="panel">
            <h3>AI workflow JSON</h3>
            <p className="note">{aiWorkflow.model} &middot; {aiWorkflow.promptTokens} in / {aiWorkflow.outputTokens} out tokens</p>
            <pre className="code-block">{aiWorkflow.workflowJson}</pre>
          </section>
        ) : null}

        {workflowRuns.length > 0 ? (
          <section className="panel">
            <h3>Run history</h3>
            <div className="run-list">
              {workflowRuns.map((run) => (
                <button key={run.id} type="button" className={`recording-chip ${run.id === selectedWorkflowRunId ? 'recording-chip--active' : ''}`} onClick={() => setSelectedWorkflowRunId(run.id)}>
                  <strong>{run.workflowName}</strong>
                  <span>{run.status} &middot; {run.completedStepCount}/{run.stepCount} steps &middot; {formatTimestamp(run.startedAtMs)}</span>
                  {run.lastError ? <span className="note">{run.lastError}</span> : null}
                </button>
              ))}
            </div>
            {selectedWorkflowRun ? (
              <div className="run-logs">
                {workflowRunLogs.map((log) => (
                  <div key={log.id} className="log-entry">
                    <span className="log-entry__type">{log.eventType}</span>
                    {log.stepIndex != null ? <span>step {log.stepIndex}</span> : null}
                    <span>{formatTimestamp(log.recordedAtMs)}</span>
                    <pre className="code-block">{JSON.stringify(parseJson(log.payloadJson), null, 2)}</pre>
                  </div>
                ))}
              </div>
            ) : null}
          </section>
        ) : null}

        <section className="panel">
          <h3>System status</h3>
          {status ? (
            <pre className="code-block">{JSON.stringify({ version: status.appVersion, transport: status.recorderTransportMode, target: status.recorderTransportTarget, protocol: status.recorderProtocolVersion, sessions: status.sessionCount, events: status.rawEventCount }, null, 2)}</pre>
          ) : <p className="loading">Loading...</p>}
          {error ? <p className="note">{error}</p> : null}
        </section>
      </details>
    </main>
  )
}

function pickExistingId(candidates: Array<number | null | undefined>, ids: number[]) {
  for (const c of candidates) { if (c != null && ids.includes(c)) return c }
  return ids[0] ?? null
}

function formatTimestamp(ts: number) { return new Date(ts).toLocaleString() }

function parseJson(s: string): Record<string, unknown> {
  try { return JSON.parse(s) } catch { return {} }
}
