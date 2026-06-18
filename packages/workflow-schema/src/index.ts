export type VariableDef = {
  name: string
  type: 'string' | 'number' | 'boolean'
  required?: boolean
}

export type SelectorAx = {
  role?: string
  subrole?: string
  title?: string
  description?: string
  identifier?: string
  valueHint?: string
  path?: number[]
}

export type SelectorSpatial = {
  anchorText?: string
  relativeTo?: 'window' | 'parent'
  rect?: { x: number; y: number; w: number; h: number }
}

export type SelectorCandidate =
  | {
      kind: 'ax'
      strategy: 'identifier' | 'title' | 'description' | 'path' | 'role'
      score: number
      reason: string
      ax: SelectorAx
    }
  | {
      kind: 'spatial'
      strategy: 'pointer'
      score: number
      reason: string
      spatial: SelectorSpatial
    }

export type Selector = {
  targetApp?: { bundleId: string }
  ax?: SelectorAx
  spatial?: SelectorSpatial
  visual?: {
    keyframeId?: string
    templateHash?: string
  }
  preferredStrategy?: SelectorCandidate['strategy']
  ranking?: SelectorCandidate[]
  fallbacks?: SelectorCandidate[]
}

export type Condition =
  | { kind: 'windowVisible'; bundleId: string; title?: string }
  | { kind: 'elementPresent'; selector: Selector }
  | { kind: 'textEquals'; selector: Selector; value: string }

export type VerificationHook = {
  condition: Condition
  timeoutMs?: number
}

export type ValueRef =
  | { kind: 'literal'; value: string }
  | { kind: 'variable'; name: string }

export type Step =
  | { kind: 'openApp'; app: string; verify?: VerificationHook[] }
  | { kind: 'focusWindow'; bundleId: string; title?: string; verify?: VerificationHook[] }
  | { kind: 'click'; selector: Selector; verify?: VerificationHook[] }
  | { kind: 'setText'; selector: Selector; value: ValueRef; verify?: VerificationHook[] }
  | { kind: 'selectMenu'; path: string[]; verify?: VerificationHook[] }
  | { kind: 'waitFor'; condition: Condition; timeoutMs: number }
  | { kind: 'assert'; condition: Condition }

export type Workflow = {
  id: string
  name: string
  inputs: VariableDef[]
  steps: Step[]
}
