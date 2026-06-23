import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";

export default tseslint.config(
  { ignores: ["dist", "src/bindings", "src-tauri"] },
  {
    files: ["**/*.{ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      // Icons flow through the central module (#284 §1) so the icon set stays
      // curated and the library is swappable from one place. `paths` blocks the
      // bare specifier; `patterns` closes the deep-import escape hatch
      // (`lucide-react/dist/...`) so the gate is airtight.
      "no-restricted-imports": [
        "error",
        {
          paths: [
            {
              name: "lucide-react",
              message:
                "Import icons from '@/components/ui/icon' instead of 'lucide-react' directly.",
            },
          ],
          patterns: [
            {
              group: ["lucide-react/*"],
              message:
                "Import icons from '@/components/ui/icon' instead of deep 'lucide-react' paths.",
            },
          ],
        },
      ],
    },
  },
  // The icon module is the one allowed place to import lucide-react.
  {
    files: ["src/components/ui/icon.ts"],
    rules: { "no-restricted-imports": "off" },
  },
);
