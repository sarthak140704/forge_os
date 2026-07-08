import type { Config } from "tailwindcss";

export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        forge: {
          bg:      "#0b0d12",
          panel:   "#12151d",
          border:  "#232732",
          fg:      "#e5e7eb",
          muted:   "#8b93a7",
          accent:  "#7c8cf8",
          success: "#34d399",
          warn:    "#fbbf24",
          err:     "#f87171",
        },
      },
      fontFamily: {
        mono: ["ui-monospace", "SFMono-Regular", "Menlo", "Consolas", "monospace"],
      },
    },
  },
  plugins: [],
} satisfies Config;
