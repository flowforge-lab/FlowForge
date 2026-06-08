import posthog from 'posthog-js'

export function initPostHog(): void {
  const key = import.meta.env.VITE_POSTHOG_KEY as string | undefined
  const host = (import.meta.env.VITE_POSTHOG_HOST as string | undefined) ?? 'https://us.i.posthog.com'

  if (!key || posthog.__loaded) return

  posthog.init(key, {
    api_host: host,
    person_profiles: 'identified_only',
    capture_pageview: true,
  })
}

export function isPostHogConfigured(): boolean {
  return Boolean(import.meta.env.VITE_POSTHOG_KEY as string | undefined)
}

export function isPostHogReady(): boolean {
  return isPostHogConfigured() && posthog.__loaded
}

export { posthog }
