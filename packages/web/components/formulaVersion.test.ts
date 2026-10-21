import { expect, test } from 'bun:test'

import { parseFormulaVersion } from './formulaVersion'

const FORMULA = `class AgentGossip < Formula
  desc "mesh network for agents"
  homepage "https://github.com/agent-habilis/agent-gossip"
  license "MIT"
  version "0.12.5"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "d516e51efea414dbdfec0003f0699ca3e088778583d87ed8fdbcc656ee4af10d"
    end
  end
end
`

test('reads the version line of the formula', () => {
  expect(parseFormulaVersion(FORMULA)).toBe('0.12.5')
})

test('a formula with no version line has no version', () => {
  expect(parseFormulaVersion(FORMULA.replace('  version "0.12.5"\n', ''))).toBeUndefined()
})

test('an error page has no version', () => {
  expect(parseFormulaVersion('404: Not Found')).toBeUndefined()
})
