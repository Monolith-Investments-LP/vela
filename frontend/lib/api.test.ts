import { describe, it, expect } from 'vitest'

describe('lib/api', () => {
  it('exports the wrappers the pages import', async () => {
    // Smoke test — verifies the module loads without throwing and the
    // public exports the frontend depends on are actually present. This
    // catches accidental deletions in refactors before they crash a page.
    const mod = await import('./api')
    expect(typeof mod.listMarkets).toBe('function')
    expect(typeof mod.getBook).toBe('function')
    expect(typeof mod.postOrder).toBe('function')
    expect(typeof mod.cancelOrder).toBe('function')
    expect(typeof mod.getPortfolio).toBe('function')
    expect(typeof mod.getPortfolioCsv).toBe('function')
    expect(typeof mod.getPerpMarkets).toBe('function')
    expect(typeof mod.getLiquidatablePositions).toBe('function')
    expect(typeof mod.getBorrowLendMarkets).toBe('function')
    expect(typeof mod.getAgentTier).toBe('function')
  })
})
