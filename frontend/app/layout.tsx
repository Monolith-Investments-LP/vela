import type { Metadata } from 'next'
import { IBM_Plex_Mono, Playfair_Display, Inter } from 'next/font/google'
import './globals.css'
import AppShell from '@/components/AppShell'

const ibmPlexMono = IBM_Plex_Mono({
  subsets: ['latin'],
  variable: '--font-mono',
  weight: ['400', '600', '700'],
  display: 'swap',
})

const playfairDisplay = Playfair_Display({
  subsets: ['latin'],
  weight: ['400', '700', '900'],
  style: ['normal', 'italic'],
  variable: '--font-playfair',
  display: 'swap',
})

const inter = Inter({
  subsets: ['latin'],
  weight: ['300', '400', '500', '600'],
  variable: '--font-inter-sans',
  display: 'swap',
})

export const metadata: Metadata = {
  title: 'Vela Exchange',
  description: 'High-performance verifiable spot DEX',
  icons: { icon: '/favicon.ico' },
}

export default function RootLayout({
  children,
}: {
  children: React.ReactNode
}) {
  return (
    <html lang="en" className={`${ibmPlexMono.variable} ${playfairDisplay.variable} ${inter.variable}`}>
      <body className="min-h-screen bg-parchment text-ink font-sans">
        <AppShell>{children}</AppShell>
      </body>
    </html>
  )
}
