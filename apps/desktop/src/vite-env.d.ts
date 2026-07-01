/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** When "1", the IPC layer uses the in-browser mock backend instead of Tauri. */
  readonly VITE_FF_MOCK?: string;
  /** When "1" (alongside VITE_FF_MOCK=1), streams at 300 ms/word so the Stop
   *  button can be tested under the mock. */
  readonly VITE_FF_MOCK_SLOW?: string;
  /** When "1", enables the FE half of the boot-timing trace (#599 item 0):
   *  reports first paint back to the Rust `[boot-trace]` on the shared clock.
   *  Always on in a dev build; pair with `FF_BOOT_TRACE=1` on the Rust side. */
  readonly VITE_FF_BOOT_TRACE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare module "@fontsource-variable/inter";
