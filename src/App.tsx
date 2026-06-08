import { Button } from '@/components/ui/button'
import { FoundationStatus } from '@/components/FoundationStatus'
import { useAppStore } from '@/store/useAppStore'

export default function App() {
  const { count, increment, reset } = useAppStore()

  return (
    <main className="min-h-screen bg-background text-foreground flex items-center justify-center p-6">
      <div className="w-full max-w-md space-y-4">
        <div className="rounded-lg border bg-card p-8 shadow-sm space-y-6">
          <div className="space-y-1">
            <h1 className="text-2xl font-semibold tracking-tight">FlowForge</h1>
            <p className="text-sm text-muted-foreground">
              M1 scaffold · React 18 · Vite · TypeScript · Tailwind · shadcn/ui · Zustand · Supabase · PostHog
            </p>
          </div>

          <FoundationStatus />

          <div className="flex items-center gap-3 border-t pt-4">
            <Button onClick={increment}>Zustand count: {count}</Button>
            <Button variant="outline" onClick={reset}>Reset</Button>
          </div>
        </div>

        <p className="text-center text-xs text-muted-foreground">
          Tauri IPC + LLM streaming → M1-Tauri PR
        </p>
      </div>
    </main>
  )
}
