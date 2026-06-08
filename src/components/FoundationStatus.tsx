import { useEffect, useState } from 'react'
import { CheckCircle2, Loader2, MinusCircle, XCircle } from 'lucide-react'
import { checkSupabaseConnection, isSupabaseConfigured } from '@/lib/supabase'
import { isPostHogConfigured, isPostHogReady } from '@/lib/posthog'

type Status = 'ok' | 'error' | 'checking' | 'unconfigured'

interface StackItem {
  id: string
  label: string
  note: string
  status: Status
  detail?: string
}

const STATIC_STACK: Omit<StackItem, 'status'>[] = [
  { id: 'react',   label: 'React 18 + Vite',       note: 'TypeScript strict mode' },
  { id: 'tw',      label: 'Tailwind + shadcn/ui',   note: 'new-york / zinc' },
  { id: 'zustand', label: 'Zustand',                note: 'client state' },
  { id: 'ai-sdk',  label: 'Vercel AI SDK',          note: 'streaming ready' },
]

function StatusIcon({ status }: Readonly<{ status: Status }>) {
  switch (status) {
    case 'ok':           return <CheckCircle2 className="h-4 w-4 shrink-0 text-emerald-500" />
    case 'error':        return <XCircle      className="h-4 w-4 shrink-0 text-destructive" />
    case 'checking':     return <Loader2      className="h-4 w-4 shrink-0 animate-spin text-muted-foreground" />
    case 'unconfigured': return <MinusCircle  className="h-4 w-4 shrink-0 text-muted-foreground/40" />
  }
}

function buildInitialItems(): StackItem[] {
  const phReady = isPostHogReady()
  const phConfigured = isPostHogConfigured()

  return [
    ...STATIC_STACK.map(item => ({ ...item, status: 'ok' as Status })),
    {
      id: 'supabase',
      label: 'Supabase',
      note: 'PostgreSQL + Auth',
      status: isSupabaseConfigured ? 'checking' : 'unconfigured' as Status,
    },
    {
      id: 'posthog',
      label: 'PostHog',
      note: 'analytics',
      status: !phConfigured
        ? 'unconfigured'
        : phReady ? 'ok' : 'error' as Status,
      detail: phReady
        ? ((import.meta.env.VITE_POSTHOG_HOST as string | undefined) ?? 'https://us.i.posthog.com')
        : phConfigured ? 'Init failed — check VITE_POSTHOG_KEY' : undefined,
    },
  ]
}

export function FoundationStatus() {
  const [items, setItems] = useState<StackItem[]>(buildInitialItems)

  useEffect(() => {
    if (!isSupabaseConfigured) return

    checkSupabaseConnection().then(({ ok, detail }) => {
      setItems(prev =>
        prev.map(item =>
          item.id === 'supabase' ? { ...item, status: ok ? 'ok' : 'error', detail } : item,
        ),
      )
    })
  }, [])

  const okCount = items.filter(i => i.status === 'ok').length

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
          Foundation Stack
        </p>
        <span className="text-xs tabular-nums text-muted-foreground">
          {okCount} / {items.length} ready
        </span>
      </div>

      <ul className="space-y-2">
        {items.map(item => (
          <li key={item.id} className="flex items-start gap-2.5">
            <div className="mt-0.5">
              <StatusIcon status={item.status} />
            </div>
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-baseline gap-x-1.5">
                <span className="text-sm font-medium leading-snug">{item.label}</span>
                <span className="text-xs text-muted-foreground">{item.note}</span>
              </div>
              {item.detail && (
                <p className="mt-0.5 text-xs text-muted-foreground/70 break-words">
                  {item.detail}
                </p>
              )}
            </div>
          </li>
        ))}
      </ul>
    </div>
  )
}
