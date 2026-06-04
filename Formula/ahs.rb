class Ahs < Formula
  desc "swarm network for agents"
  homepage "https://github.com/agent-habilis/swarm"
  license "MIT"
  version "0.4.3"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "1db8642f7b65963043c53a9c7c9135998519a29b0cd71ce6b44eb2fa6aa59c59"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "d588285bc42e56ee4d01f3e86303cafe807c5d614e1d0861aec4e2c917dfb91b"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ahs-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "3437301d2f45977c69ff553401a50ae326ccf9f96bcbfec4ddb2a27f9819d5ac"
    end
  end

  def install
    bin.install "ahs"
  end

  test do
    assert_match "ahs", shell_output("#{bin}/ahs --version")
  end
end
