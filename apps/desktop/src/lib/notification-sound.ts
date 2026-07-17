// Short notification chime (#994), gated behind the "Sound" pref by lib/notify.ts.
// A WebAudio-generated two-tone blip rather than a bundled audio asset: zero bundle
// impact, no file to load, and no autoplay-policy fight (it plays in response to a
// backend event, and a muted/suspended context simply produces nothing). Best-effort
// throughout — any failure (no AudioContext, suspended, blocked) is swallowed so a
// notification never throws.

let ctx: AudioContext | null = null;

function audioContext(): AudioContext | null {
  if (typeof window === "undefined") return null;
  const Ctor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext })
      .webkitAudioContext;
  if (!Ctor) return null;
  if (!ctx) ctx = new Ctor();
  return ctx;
}

/** Play a brief, soft two-note chime. No-op if WebAudio is unavailable. */
export function playChime(): void {
  try {
    const ac = audioContext();
    if (!ac) return;
    // A user-gesture-less context can start "suspended"; nudge it, ignoring failure.
    if (ac.state === "suspended") void ac.resume();

    const now = ac.currentTime;
    const gain = ac.createGain();
    gain.connect(ac.destination);
    // Low peak + quick decay: a calm blip, not an alarm.
    gain.gain.setValueAtTime(0.0001, now);
    gain.gain.exponentialRampToValueAtTime(0.08, now + 0.01);
    gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.28);

    // Two rising notes (E6 → A6) for a pleasant "ding".
    [880, 1320].forEach((freq, i) => {
      const osc = ac.createOscillator();
      osc.type = "sine";
      osc.frequency.value = freq;
      osc.connect(gain);
      const start = now + i * 0.09;
      osc.start(start);
      osc.stop(start + 0.2);
    });
  } catch {
    // Best-effort: a sound failure must never break a notification.
  }
}
