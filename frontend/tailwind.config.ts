import type { Config } from 'tailwindcss'

const config: Config = {
  content: [
    './app/**/*.{ts,tsx}',
    './components/**/*.{ts,tsx}',
    './lib/**/*.{ts,tsx}',
  ],
  theme: {
    // Mobile-first breakpoints. Explicitly listed so the config is
    // self-documenting and the mobile-first audit has a single source
    // of truth (rather than inheriting Tailwind's implicit defaults).
    screens: {
      xs: '380px',
      sm: '640px',
      md: '768px',
      lg: '1024px',
      xl: '1280px',
      '2xl': '1536px',
    },
    borderRadius: {
      none: '0px',
      sm: '0px',
      DEFAULT: '0px',
      md: '0px',
      lg: '0px',
      xl: '0px',
      '2xl': '0px',
      '3xl': '0px',
      full: '9999px',
    },
    extend: {
      colors: {
        parchment: '#0C0C0C',
        canvas: '#111110',
        vellum: '#191915',
        ink: '#E8E4D8',
        brown: 'rgba(232,228,216,0.35)',
        ochre: '#E8E4D8',
        fresco: '#8A7F6E',
        terra: '#CC3333',
        sage: '#6B8A5A',
        violet: '#E8E4D8',
        border: 'rgba(232,228,216,0.08)',
      },
      fontFamily: {
        sans: ['var(--font-inter-sans)', 'Inter', 'sans-serif'],
        mono: ['"Courier New"', 'monospace'],
      },
      boxShadow: {
        card: 'none',
        'card-hover': 'none',
        modal: 'none',
      },
    },
  },
  plugins: [],
}

export default config
