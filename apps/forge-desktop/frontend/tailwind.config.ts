import type { Config } from "tailwindcss";

export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        forge: {
          // Base surfaces — true dark with a faint neutral-blue cast.
          bg:      "#0a0c11",
          bgSoft:  "#0d1016",
          panel:   "#12151d",
          panel2:  "#171b25",
          card:    "#12151d",
          // Borders / dividers.
          border:  "#222634",
          borderStrong: "#2d3342",
          // Text.
          fg:      "#f4f5f7",
          muted:   "#8b93a7",
          faint:   "#5b6273",
          // Accents.
          accent:  "#7c8cf8",
          accent2: "#a78bfa",
          // Functional.
          success: "#34d399",
          warn:    "#fbbf24",
          err:     "#f87171",
          info:    "#5eb3ff",
        },
      },
      fontFamily: {
        sans: [
          "Inter",
          "Inter var",
          "system-ui",
          "-apple-system",
          "Segoe UI Variable",
          "Segoe UI",
          "Roboto",
          "Helvetica Neue",
          "Arial",
          "sans-serif",
        ],
        mono: ["ui-monospace", "SFMono-Regular", "JetBrains Mono", "Menlo", "Consolas", "monospace"],
      },
      letterSpacing: {
        tighter2: "-0.015em",
      },
      boxShadow: {
        card: "0 1px 2px rgba(0,0,0,0.4), 0 8px 24px -12px rgba(0,0,0,0.5)",
        pop:  "0 12px 40px -12px rgba(0,0,0,0.7)",
        glow: "0 0 0 1px rgba(124,140,248,0.4), 0 0 24px -6px rgba(124,140,248,0.45)",
      },
      backgroundImage: {
        "forge-radial":
          "radial-gradient(1200px 600px at 20% -10%, rgba(124,140,248,0.08), transparent 60%), radial-gradient(900px 500px at 100% 0%, rgba(167,139,250,0.06), transparent 55%)",
        "accent-grad": "linear-gradient(135deg, #7c8cf8 0%, #a78bfa 100%)",
      },
      keyframes: {
        "fade-in": {
          "0%": { opacity: "0", transform: "translateY(4px)" },
          "100%": { opacity: "1", transform: "translateY(0)" },
        },
        "pulse-dot": {
          "0%, 100%": { opacity: "1" },
          "50%": { opacity: "0.35" },
        },
        shimmer: {
          "100%": { transform: "translateX(100%)" },
        },
      },
      animation: {
        "fade-in": "fade-in 160ms ease-out",
        "pulse-dot": "pulse-dot 1.4s ease-in-out infinite",
      },
    },
  },
  plugins: [],
} satisfies Config;
