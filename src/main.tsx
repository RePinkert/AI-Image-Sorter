import { Component, StrictMode, type ErrorInfo, type ReactNode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App.tsx'
import { exportDiagnostics, trackError } from './telemetry'

interface ErrorBoundaryState {
  failed: boolean
}

class ErrorBoundary extends Component<{ children: ReactNode }, ErrorBoundaryState> {
  state: ErrorBoundaryState = { failed: false }

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { failed: true }
  }

  componentDidCatch(error: Error, _info: ErrorInfo) {
    trackError('unhandled_error', error, { source: 'error_boundary' })
  }

  render() {
    if (this.state.failed) {
      return (
        <main className="fatal-error" role="alert">
          <h1>界面发生错误</h1>
          <p>当前操作已停止。可导出脱敏诊断后重新加载界面。</p>
          <div className="row">
            <button type="button" onClick={exportDiagnostics}>导出脱敏诊断</button>
            <button type="button" onClick={() => window.location.reload()}>重新加载</button>
          </div>
        </main>
      )
    }
    return this.props.children
  }
}

window.addEventListener('error', (event) => {
  trackError('unhandled_error', event.error ?? event.message, { source: 'window_error' })
})

window.addEventListener('unhandledrejection', (event) => {
  trackError('unhandled_error', event.reason, { source: 'unhandled_rejection' })
})

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </StrictMode>,
)
