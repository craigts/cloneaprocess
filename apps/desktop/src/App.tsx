import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'

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
}

export function App() {
  const [status, setStatus] = useState<BackendStatus | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false

    invoke<BackendStatus>('system_status')
      .then((response) => {
        if (!cancelled) {
          setStatus(response)
          setError(null)
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setStatus(browserFallbackStatus)
          setError(String(err))
        }
      })

    return () => {
      cancelled = true
    }
  }, [])

  return (
    <main className="shell">
      <section className="hero">
        <p className="eyebrow">macOS-first automation workbench</p>
        <h1>Clone a desktop workflow before we teach it to run.</h1>
        <p className="lede">
          This shell is wired to the first Rust command path and is ready for recorder,
          workflow, and runner surfaces to land on top.
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
              <dt>Started</dt>
              <dd>{new Date(status.startedAtMs).toISOString()}</dd>
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
              <dt>Storage schema</dt>
              <dd>v{status.storageSchemaVersion}</dd>
            </div>
            <div>
              <dt>Workflow IR</dt>
              <dd>v{status.workflowIrVersion}</dd>
            </div>
          </dl>
        ) : (
          <p className="loading">Connecting to Rust core...</p>
        )}

        {error ? <p className="note">{error}</p> : null}
      </section>

      <section className="panel panel--roadmap">
        <header className="panel-header">
          <div>
            <p className="panel-kicker">Next surfaces</p>
            <h2>Implementation track</h2>
          </div>
        </header>

        <ul className="roadmap">
          <li>Permissions onboarding with recorder and runner availability checks.</li>
          <li>Session timeline with keyframes stored on disk and indexed in SQLite.</li>
          <li>Workflow draft panel backed by semantic action compilation.</li>
        </ul>
      </section>
    </main>
  )
}
