class AhS < Formula
  desc "swarm network for agents"
  homepage "https://github.com/agent-habilis/swarm"
  license "MIT"
  version "0.4.4"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ah-s-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "037e4b7f46e5c5d79819b0deef806dac8f7f3de3ec7a64fc339c0d5cefa870d8"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ah-s-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "e7e9559b7dad43ea1da04d13595101f374c7aa2853696bad45eefecadd93737e"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/swarm/releases/download/v#{version}/ah-s-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "eb11a1e783ac2ac3ca98a64bcafc81e0cd0998411e682088ed8511e9b4caca1d"
    end
  end

  def install
    bin.install "ah-s"
    man1.install Dir["man/*.1"]
  end

  test do
    assert_match "ah-s", shell_output("#{bin}/ah-s --version")
  end
end
