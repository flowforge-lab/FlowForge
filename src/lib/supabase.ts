import { createClient, type SupabaseClient } from '@supabase/supabase-js'

const url = import.meta.env.VITE_SUPABASE_URL as string | undefined
const anonKey = import.meta.env.VITE_SUPABASE_PUBLISHABLE_KEY as string | undefined

export const isSupabaseConfigured = Boolean(url && anonKey)

// null when env vars are absent — guard with isSupabaseConfigured before use
export const supabase: SupabaseClient | null = isSupabaseConfigured
  ? createClient(url!, anonKey!)
  : null

export async function checkSupabaseConnection(): Promise<{ ok: boolean; detail: string }> {
  if (!isSupabaseConfigured || !url || !anonKey) {
    return { ok: false, detail: 'Add VITE_SUPABASE_URL + VITE_SUPABASE_PUBLISHABLE_KEY to .env' }
  }

  // Only JWT anon keys (eyJ...) work with the PostgREST REST layer.
  // sb_publishable_ / sb_secret_ / UUIDs are rejected by the API gateway.
  if (!anonKey.startsWith('eyJ')) {
    return {
      ok: false,
      detail: 'Use the anon JWT key (eyJ…) — Dashboard → Settings → API → anon',
    }
  }

  try {
    // Supabase's Kong gateway requires the apikey header on ALL endpoints, including health.
    // One request is enough: 200 = up + key valid, 401 = bad key, 503/404 = paused.
    const res = await fetch(`${url}/auth/v1/health`, {
      headers: { apikey: anonKey },
    })

    if (res.status === 401) {
      return { ok: false, detail: 'Invalid anon key — re-copy from Dashboard → Settings → API' }
    }
    if (res.status === 503 || res.status === 404) {
      return { ok: false, detail: 'Project is paused — click "Restart project" in Dashboard' }
    }
    if (!res.ok) {
      return { ok: false, detail: `Supabase returned HTTP ${res.status}` }
    }

    return { ok: true, detail: url }
  } catch (err) {
    return {
      ok: false,
      detail: err instanceof Error ? err.message : 'Network error — check your connection',
    }
  }
}
