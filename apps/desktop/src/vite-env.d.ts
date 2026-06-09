/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** When "1", the IPC layer uses the in-browser mock backend instead of Tauri. */
  readonly VITE_FF_MOCK?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
