export type VariableDef = {
  name: string
  type: 'string' | 'number' | 'boolean'
  required?: boolean
}

export type Selector = {
  targetApp?: { bundleId: string }
  ax?: {
    role?: string
    subrole?: string
    title?: string
    description?: string
    identifier?: string
    valueHint?: string
    path?: number[]
  }
  spatial?: {
    anchorText?: string
    relativeTo?: 'window' | 'parent'
    rect?: { x: number; y: number; w: number; h: number }
  }
  visual?: {
    keyframeId?: string
    templateHash?: string
  }
}

export type Condition =
  | { kind: 'windowVisible'; bundleId: string; title?: string }
  | { kind: 'elementPresent'; selector: Selector }
  | { kind: 'textEquals'; selector: Selector; value: string }

export type ValueRef =
  | { kind: 'literal'; value: string }
  | { kind: 'variable'; name: string }

export type Step =
  | { kind: 'openApp'; app: string }
  | { kind: 'focusWindow'; bundleId: string; title?: string }
  | { kind: 'click'; selector: Selector }
  | { kind: 'setText'; selector: Selector; value: ValueRef }
  | { kind: 'selectMenu'; path: string[] }
  | { kind: 'waitFor'; condition: Condition; timeoutMs: number }
  | { kind: 'assert'; condition: Condition }

export type Workflow = {
  id: string
  name: string
  inputs: VariableDef[]
  steps: Step[]
}

