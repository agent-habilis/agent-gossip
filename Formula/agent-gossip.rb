class AgentGossip < Formula
  desc "mesh network for agents"
  homepage "https://github.com/agent-habilis/agent-gossip"
  license "MIT"
  version "0.7.1"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "b2c9c1418aad118639c4e4dd9074233484b887cf091c850617633de555dc4d79"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "48de5e263a65dbdf628cd571bdfea3127bac6b91800ce2c700692d0ebb507236"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-gossip/releases/download/v#{version}/agent-gossip-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "acfd79bb004095be1542b47b9640bd7098c7f9c082e18a093070bb7cf4f05ba0"
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
