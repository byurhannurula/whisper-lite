// Flat config (ESLint 9+).
//
// Deliberately close to the recommended sets. The frontend is three files of plain TypeScript
// with no framework, so an elaborate rule set would be more configuration than code. Formatting
// is Prettier's job — `eslintConfigPrettier` last turns off every rule that would argue with it.

import js from "@eslint/js";
import tseslint from "typescript-eslint";
import eslintConfigPrettier from "eslint-config-prettier";

export default tseslint.config(
  {
    // Not linted: build output, dependencies, Rust's target dir, and Tauri's generated schemas.
    ignores: ["dist/", "node_modules/", "src-tauri/target/", "src-tauri/gen/"],
  },

  js.configs.recommended,
  ...tseslint.configs.recommended,

  {
    files: ["src/**/*.ts"],
    languageOptions: {
      parserOptions: {
        project: "./tsconfig.json",
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      // The Tauri command layer returns `unknown`-shaped payloads that get asserted at the
      // boundary. Warn rather than error so it stays visible without blocking a build.
      "@typescript-eslint/no-explicit-any": "warn",
      // `$<T>()` is a deliberate non-null assertion helper for elements the HTML guarantees.
      "@typescript-eslint/no-non-null-assertion": "off",
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_" }],
    },
  },

  {
    // Node scripts, not browser code: different globals, and no TypeScript project to attach to.
    files: ["scripts/**/*.mjs", "*.config.js", "vite.config.ts"],
    languageOptions: {
      globals: { process: "readonly", console: "readonly", URL: "readonly" },
    },
  },

  eslintConfigPrettier,
);
