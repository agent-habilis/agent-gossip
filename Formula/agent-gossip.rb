class AgentGossip < Formula
  desc "mesh network for agents"
  homepage "https://github.com/agent-habilis/agent-gossip"
  license "MIT"
  version "0.7.4"

  # The release workflow rewrites every version and digest below, matching a
  # `sha256` line only where it directly follows its `url`. Nothing may be put
  # between the two, or that pair keeps its stale digest through the bump and
  # ships a formula that cannot verify.
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "25f4095cb5fab2977cc0022e92f7c2a82682c7824b23af4ab9b666a7db08693a"
    else
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "a4803a67c5887cf5baf46f7b8e8f2bc4f2aacb513083bb51074c97cd400b7ea0"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "2cfa355c1877c150e50072a37a1db3e6073c70632b20d5602ab84ebd96a4448e"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "c281abf620ee97da38eb633289acdc778bcb2f02cf761acc665048b9b1467e53"
    end
  end

  def install
    bin.install "agent-gossip"
    man1.install Dir["man/*.1"]
  end

  test do
    assert_match "agent-gossip", shell_output("#{bin}/agent-gossip --version")
  end
end
