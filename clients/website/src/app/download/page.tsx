import Link from "next/link";
import { Navbar } from "@/components/Navbar";
import { Footer } from "@/components/Footer";

export const metadata = {
  title: "Mine Equium with your GPU · install guide",
  description:
    "Mine $EQM with the open-source GPU miner. Cross-platform via wgpu — Metal, Vulkan, DX12. Build from source in 5 minutes on macOS, Linux, or Windows. CPU fallback included.",
};

export default function DownloadPage() {
  return (
    <main>
      <Navbar />
      <div className="pt-32 pb-20 px-6">
        <div className="max-w-3xl mx-auto">
          <div className="text-[11px] font-mono uppercase tracking-[0.2em] text-[var(--color-rose)] mb-3 font-semibold">
            Mine Equium
          </div>
          <h1 className="text-[40px] md:text-[52px] font-black tracking-[-0.025em] leading-[1.05] mb-5">
            Mine $EQM with your GPU.
          </h1>
          <p className="text-[17px] leading-[1.6] text-[var(--color-fg-dim)] max-w-2xl mb-4">
            The GPU miner is the fastest path to earning EQM. Single
            Rust binary, no Electron, no installer popups, no CUDA, no
            proprietary driver — cross-platform via{" "}
            <Code>wgpu</Code> (Metal on macOS, Vulkan on Linux/Windows,
            DX12 on Windows). Any modern GPU works.
          </p>
          <p className="text-[15px] leading-[1.6] text-[var(--color-fg-dim)] max-w-2xl mb-4">
            No GPU? The{" "}
            <a href="#cpu" className="text-[var(--color-rose)] hover:underline">
              CPU fallback
            </a>{" "}
            still earns blocks — Equihash is memory-bound so commodity
            hardware stays competitive — but if you have a GPU, use it.
          </p>
          <p className="text-[15px] leading-[1.6] text-[var(--color-fg-dim)] max-w-2xl mb-10">
            Just want to try mining without installing anything? Use the{" "}
            <Link
              href="/mine"
              className="text-[var(--color-rose)] font-semibold hover:underline"
            >
              browser miner
            </Link>
            . Same protocol, slower because of WASM overhead, but zero
            setup.
          </p>

          {/* OS picker — each section installs the GPU miner */}
          <div className="grid grid-cols-2 sm:grid-cols-4 gap-3 mb-10">
            <OsCard
              label="Rent a GPU"
              hint="vast.ai / Runpod · easiest"
              href="#cloud"
            />
            <OsCard
              label="macOS"
              hint="Metal · Apple Silicon or Intel"
              href="#macos"
            />
            <OsCard
              label="Linux"
              hint="Vulkan · NVIDIA, AMD, Intel"
              href="#linux"
            />
            <OsCard
              label="Windows"
              hint="DX12 or Vulkan via WSL2"
              href="#windows"
            />
          </div>

          {/* Cloud GPU rental — the easy path */}
          <Section id="cloud" title="Rent a GPU and mine in 3 steps.">
            <Callout>
              <strong>No GPU at home? Rent one for ~$0.20/hr.</strong>{" "}
              <a
                href="https://cloud.vast.ai/?ref_id=270998"
                target="_blank"
                rel="noreferrer noopener"
                className="text-[var(--color-rose)] hover:underline"
              >
                vast.ai
              </a>{" "}
              and{" "}
              <a
                href="https://runpod.io"
                target="_blank"
                rel="noreferrer noopener"
                className="text-[var(--color-rose)] hover:underline"
              >
                Runpod
              </a>{" "}
              both rent NVIDIA cards by the hour with one-click Ubuntu
              + CUDA images. We don't use CUDA directly — the miner
              talks to the GPU via Vulkan, which NVIDIA's driver
              supports natively — so any of the standard "PyTorch /
              CUDA 12" templates work out of the box.
            </Callout>

            <Block label="1 · Pick an instance">
              <P>
                Equihash 96,5 is memory-bandwidth bound, not
                compute-bound. That changes what you want on the
                marketplace — cheaper mid-range cards usually win on
                $/hashrate over flagship GPUs because the flagship's
                extra ALU lanes go mostly unused.
              </P>
              <ul className="list-disc pl-6 space-y-2.5 text-[14.5px] leading-[1.65] text-[var(--color-fg-dim)] my-5">
                <li>
                  <strong>GPU model.</strong> Sweet spots: RTX 3060
                  (12 GB or 8 GB), RTX 4060, RTX 3070, RTX 4070. Any
                  Pascal-or-newer NVIDIA with ≥6 GB VRAM works. RTX
                  3090 / 4090 / A100 etc. mine fine but you pay 3-5×
                  the rental rate for ~2× the hashrate — bad
                  $/hashrate.
                </li>
                <li>
                  <strong>GPU count: 1 is enough.</strong> Our miner
                  uses one device. Multi-GPU instances waste money.
                </li>
                <li>
                  <strong>VRAM ≥ 6 GB.</strong> The full-GPU pipeline
                  allocates ~95 MB, so 4 GB cards work, but Vulkan
                  init reserves more — 6 GB is the safe floor.
                </li>
                <li>
                  <strong>Driver version: avoid 575.x.</strong> The
                  marketplace card shows the host's NVIDIA driver
                  (look for "CUDA: 12.x · Driver: 535.x" or similar).
                  535 / 545 / 550 LTS branches are golden. 575.x has
                  a SPIR-V compiler crash —{" "}
                  <Code>cloud-mine.sh</Code> auto-handles it, but
                  you'll save a reboot by skipping these listings.
                </li>
                <li>
                  <strong>Disk ≥ 10 GB.</strong> The bootstrap
                  script's pre-flight bails on anything smaller.
                </li>
                <li>
                  <strong>RAM ≥ 4 GB.</strong> Less and cargo will
                  OOM during the build. Our script auto-adds swap
                  below 2 GB but it's slow.
                </li>
                <li>
                  <strong>Network: 50 Mbps+.</strong> We barely
                  transfer anything during mining, but the
                  one-time repo clone + cargo deps download a few
                  hundred MB.
                </li>
                <li>
                  <strong>On-demand &gt; interruptible.</strong> Spot
                  / interruptible instances get killed when the host
                  needs them, mid-block. Pay the 20-30% premium for
                  reliability.
                </li>
                <li>
                  <strong>Verified host badge.</strong> vast.ai marks
                  hosts that pass their hardware checks. Stick to
                  those — saves hunting for "why is verify failing".
                </li>
                <li>
                  <strong>OS image: Ubuntu 22.04.</strong> Any of the
                  PyTorch / CUDA / "ML standard" base templates work.
                  We don't use the Python or PyTorch stack but they
                  ship the right NVIDIA driver setup.
                </li>
              </ul>
              <P>
                Once rented, connect via SSH using the command
                vast.ai prints in the instance dashboard. The very
                next step is the bootstrap script below.
              </P>
            </Block>

            <Block label="2 · Run the bootstrap script">
              <Pre>{`curl -fsSL https://raw.githubusercontent.com/HannaPrints/equium/master/scripts/cloud-mine.sh -o cloud-mine.sh
chmod +x cloud-mine.sh
./cloud-mine.sh`}</Pre>
              <P>
                The script installs Rust, the Vulkan loader, and the
                Solana CLI if they're missing, clones the repo, builds{" "}
                <Code>equium-gpu-miner</Code>, generates a mining
                keypair, and runs <Code>verify</Code> so you can see
                your rented GPU is detected. Then it prints a Solana
                address and waits.
              </P>
            </Block>

            <Block label="3 · Fund + mine">
              <P>
                Send ~0.01 SOL to the printed address for tx fees, and
                paste a Helius RPC URL when prompted. The script polls
                the balance until the funds land, then execs the miner.
                That's it.
              </P>
              <Callout tone="dim">
                <strong>The script handles every weird case for you.</strong>{" "}
                The miner's backend auto-probe runs each candidate in
                a subprocess (so a driver SIGSEGV in{" "}
                <Code>libnvidia-glvkspirv.so</Code> kills only the
                probe child, not your shell), and{" "}
                <Code>cloud-mine.sh</Code> detects NVIDIA 575.x via{" "}
                <Code>nvidia-smi</Code> and offers a one-keypress{" "}
                <Code>apt install nvidia-driver-535-server</Code> + reboot
                — the permanent fix for that branch's SPIR-V crash. After
                reboot, rerun the script and it skips straight to mining
                (RPC URL is saved at <Code>~/.config/equium/rpc</Code>).
                Override the backend choice via{" "}
                <Code>EQUIUM_BACKEND=vulkan|gl|metal|dx12</Code> if you
                ever need to.
              </Callout>
              <Callout tone="dim">
                <strong>Keep the keypair if you want to keep mining.</strong>{" "}
                vast.ai instances are ephemeral — when you stop the
                rental, the box goes away. Copy{" "}
                <Code>~/.config/solana/id.json</Code> off the box
                before destroying the instance, or your mined EQM stays
                tied to a key only that instance held. The{" "}
                <Link href="/mine" className="text-[var(--color-rose)] hover:underline">
                  browser miner
                </Link>{" "}
                gives you the same wallet on any device for free, FYI.
              </Callout>
            </Block>
          </Section>

          {/* Common prerequisites */}
          <section className="mb-12">
            <h2 className="text-[22px] font-bold tracking-[-0.015em] mb-3">
              What you'll need
            </h2>
            <ul className="list-disc pl-6 space-y-2 text-[14.5px] leading-[1.65] text-[var(--color-fg-dim)]">
              <li>
                A Solana keypair file. Generate one with{" "}
                <Code>solana-keygen new -o ~/.config/solana/id.json</Code>{" "}
                (the Solana CLI install instructions are part of each section
                below).
              </li>
              <li>
                A small amount of SOL for transaction fees in that keypair's
                address. Roughly 0.01 SOL covers a few hours of mining.
              </li>
              <li>
                A Solana RPC endpoint. A free Helius key is fine —{" "}
                <Link
                  href="/docs/rpc"
                  className="text-[var(--color-rose)] hover:underline"
                >
                  5-minute setup
                </Link>
                . The default public mainnet endpoint will rate-limit you out
                of meaningful mining within seconds.
              </li>
            </ul>
          </section>

          {/* macOS */}
          <Section id="macos" title="macOS">
            <P>
              Native macOS — Apple Silicon or Intel, both work. Open Terminal:
            </P>
            <Block label="1 · Install Rust">
              <Pre>{`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"`}</Pre>
            </Block>
            <Block label="2 · Install the Solana CLI (for keypair generation)">
              <Pre>{`sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"`}</Pre>
            </Block>
            <Block label="3 · Generate a mining keypair">
              <Pre>{`solana-keygen new -o ~/.config/solana/id.json --no-bip39-passphrase
solana-keygen pubkey ~/.config/solana/id.json   # send a bit of SOL here`}</Pre>
            </Block>
            <Block label="4 · Build + run the GPU miner">
              <Pre>{`git clone https://github.com/HannaPrints/equium.git
cd equium
cargo build --release -p equium-gpu-miner

# Quick sanity check — auto-probes Metal and confirms the shader
# matches the CPU reference byte-for-byte.
./target/release/equium-gpu-miner verify

./target/release/equium-gpu-miner mine \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json`}</Pre>
              <P>
                Apple Silicon talks to the GPU over Metal; everything
                stays on-device, no driver install. Once it's mining,
                see{" "}
                <a href="#advanced" className="text-[var(--color-rose)] hover:underline">
                  Tune your GPU miner
                </a>{" "}
                for benchmarks and the v0.2 full-GPU mode.
              </P>
            </Block>
          </Section>

          {/* Linux */}
          <Section id="linux" title="Linux">
            <P>
              Tested on Ubuntu 22.04, Debian 12, Arch, and Fedora. Other
              distros work the same — adjust the package manager call.
            </P>
            <Block label="1 · Install build tools">
              <Pre>{`# Debian / Ubuntu
sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev curl git

# Arch
sudo pacman -S --needed base-devel openssl curl git

# Fedora
sudo dnf install -y @development-tools openssl-devel curl git`}</Pre>
            </Block>
            <Block label="2 · Install Rust">
              <Pre>{`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"`}</Pre>
            </Block>
            <Block label="3 · Install the Solana CLI">
              <Pre>{`sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"`}</Pre>
            </Block>
            <Block label="4 · Generate a keypair">
              <Pre>{`solana-keygen new -o ~/.config/solana/id.json --no-bip39-passphrase
solana-keygen pubkey ~/.config/solana/id.json`}</Pre>
            </Block>
            <Block label="5 · Build + run the GPU miner">
              <Pre>{`# Vulkan loader (wgpu's preferred path on Linux). If it's missing
# the miner falls back to GL automatically — but Vulkan is faster.
# Debian / Ubuntu
sudo apt install -y libvulkan1 vulkan-tools
# Arch
sudo pacman -S vulkan-icd-loader vulkan-tools
# Fedora
sudo dnf install -y vulkan-loader vulkan-tools

git clone https://github.com/HannaPrints/equium.git
cd equium
cargo build --release -p equium-gpu-miner

# Sanity check. The miner self-probes Vulkan → GL in subprocesses
# so a buggy driver kills the probe child, not your terminal.
./target/release/equium-gpu-miner verify

./target/release/equium-gpu-miner mine \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json`}</Pre>
              <P>
                Works on NVIDIA, AMD, and Intel GPUs that support
                Vulkan 1.1+ (or GL 4.5 as fallback). If{" "}
                <Code>vulkaninfo</Code> lists your adapter, the miner
                will pick it up. Once mining works, see{" "}
                <a href="#advanced" className="text-[var(--color-rose)] hover:underline">
                  Tune your GPU miner
                </a>
                .
              </P>
            </Block>
          </Section>

          {/* Windows */}
          <Section id="windows" title="Windows">
            <Callout>
              <strong>WSL2 is the path of least resistance.</strong> Native
              Windows + Rust + Solana CLI is theoretically possible but
              accumulates papercuts fast (path handling, OpenSSL, line
              endings). Run Linux inside Windows via WSL2 and skip them.
            </Callout>

            <Block label="1 · Install WSL2 + Ubuntu (one-time)">
              <P>
                Open <strong>PowerShell as Administrator</strong> and run:
              </P>
              <Pre>{`wsl --install -d Ubuntu`}</Pre>
              <P>
                Reboot when prompted, then open the new "Ubuntu" app from
                the Start menu and set a UNIX username + password. You're
                now inside Linux — everything below runs in the Ubuntu
                terminal, not PowerShell.
              </P>
            </Block>

            <Block label="2 · Install build tools (inside WSL)">
              <Pre>{`sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev curl git`}</Pre>
            </Block>

            <Block label="3 · Install Rust">
              <Pre>{`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"`}</Pre>
            </Block>

            <Block label="4 · Install the Solana CLI">
              <Pre>{`sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"`}</Pre>
            </Block>

            <Block label="5 · Generate a keypair + build the GPU miner">
              <Pre>{`solana-keygen new -o ~/.config/solana/id.json --no-bip39-passphrase
solana-keygen pubkey ~/.config/solana/id.json    # send SOL here

# WSL2 exposes your Windows GPU via Vulkan (NVIDIA + AMD have full
# support; Intel works on recent drivers). Install Vulkan dev headers:
sudo apt install -y libvulkan1 vulkan-tools

git clone https://github.com/HannaPrints/equium.git
cd equium
cargo build --release -p equium-gpu-miner

# Sanity check. Auto-probes Vulkan → GL in subprocesses so a buggy
# driver kills the probe, not your shell.
./target/release/equium-gpu-miner verify

./target/release/equium-gpu-miner mine \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json`}</Pre>
            </Block>

            <Callout tone="dim">
              <strong>Native Windows build?</strong> Run the same{" "}
              <Code>cargo build</Code> from PowerShell after installing
              rustup-init.exe, Build Tools for Visual Studio (C++
              workload), and the Solana Windows installer. wgpu picks
              up DX12 automatically. File a GitHub issue if something
              specific breaks and we'll document a workaround.
            </Callout>
          </Section>

          {/* Tune your GPU miner */}
          <Section id="advanced" title="Tune your GPU miner">
            <P>
              Once <Code>mine</Code> is running, the commands below let
              you verify the shader on your specific driver, benchmark
              throughput, and opt into the v0.2 all-on-GPU pipeline.
            </P>

            <Block label="Verify the shader is correct for your driver">
              <Pre>{`# Pure-Rust port checked against blake2b_simd — always passes,
# catches WGSL logic bugs without needing a GPU.
./target/release/equium-gpu-miner verify-cpu

# Real on-device test. The miner auto-probes wgpu backends
# (Vulkan → GL) in subprocesses, so a driver SIGSEGV kills the
# probe child instead of you. Expected output on a healthy box:
#
#   backend: probing vulkan… OK
#     ↳ NVIDIA GeForce RTX 4090 (Vulkan, wg=128)
#   GPU backend: NVIDIA GeForce RTX 4090 (Vulkan, wg=128)
#   leaves: 2048   gpu: 12ms   cpu: 4ms   match: 2048/2048
#   ✓ GPU output matches CPU reference byte-for-byte.
./target/release/equium-gpu-miner verify`}</Pre>
              <P>
                If <Code>verify</Code> reports a byte mismatch, open a
                GitHub issue with the first-mismatch hex output and
                your GPU / OS — we want to know about every driver
                case. If a backend gets skipped (
                <Code>FAILED (killed by signal — driver crash)</Code>),
                the miner moves on automatically; the troubleshooting
                line points at the underlying fix (usually a driver
                downgrade).
              </P>
            </Block>

            <Block label="Benchmark throughput">
              <Pre>{`./target/release/equium-gpu-miner bench`}</Pre>
              <P>
                Reports BLAKE2b throughput at full Equihash 96,5 width.
                Useful for confirming a real adapter was picked up, not
                a software fallback.
              </P>
            </Block>

            <Block label="Full-GPU mode (v0.2, opt-in)">
              <Pre>{`# Sanity-check first — runs the GPU Wagner pipeline alongside
# the CPU reference on the same nonces and asserts they agree.
./target/release/equium-gpu-miner verify-rounds --nonces 4

# Then mine with everything on GPU.
./target/release/equium-gpu-miner mine --full-gpu \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json`}</Pre>
              <P>
                v0.2 moves the full Wagner solver onto the GPU — all
                five rounds plus solution scan run on-device, with
                only tx submission left on CPU. The algorithm is
                byte-for-byte validated against the CPU solver at all
                5 rounds (see <Code>cargo test</Code> in{" "}
                <Code>clients/gpu-miner</Code>); the on-device
                verification step above checks your specific
                driver/adapter before you commit.
              </P>
            </Block>

            <Callout tone="dim">
              v0.1 hybrid (GPU leaves + CPU Wagner) is the default and
              battle-tested. v0.2 full-GPU is shipping for early
              testers — run <Code>verify-rounds</Code> and report any
              disagreement before relying on it. v0.3 + v0.4 bring
              the same WGSL kernels to the browser miner via WebGPU,
              with an automatic three-tier fallback (Full-GPU →
              Hybrid → CPU) per browser capability.{" "}
              <a
                href="https://github.com/HannaPrints/equium/tree/master/clients/gpu-miner"
                className="text-[var(--color-rose)] hover:underline"
                target="_blank"
                rel="noreferrer noopener"
              >
                Source + roadmap
              </a>
              .
            </Callout>
          </Section>

          {/* CPU fallback */}
          <Section id="cpu" title="No GPU? CPU still earns blocks.">
            <P>
              Equihash 96,5 is memory-bound, so the protocol stays
              ASIC-resistant and commodity CPUs stay viable miners.
              The auto-retargeter scales difficulty with total
              hashrate, so a pure-CPU miner keeps winning a share of
              blocks — your share just tracks your share of total
              network hashrate, like any PoW chain.
            </P>
            <Block label="Build + run the CPU miner">
              <Pre>{`# Prereqs (Rust + Solana CLI + keypair) from the section above.
cd equium
cargo build --release -p equium-cli-miner

./target/release/equium-miner \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json \\
  --threads $(nproc 2>/dev/null || sysctl -n hw.physicalcpu)`}</Pre>
            </Block>
            <P>
              One solver thread per physical core by default. Override
              with <Code>--threads N</Code> if you want to leave some
              cores free for other work. Each thread independently
              grinds nonces; first to find a below-target solution
              wins the round.
            </P>
          </Section>

          {/* Performance notes */}
          <Section id="perf" title="Performance notes">
            <P>
              The auto-retargeter brings difficulty up as more miners join
              the network. Expect your hashrate, in absolute terms, to look
              fine while your share of blocks shrinks — that's the network
              working as designed.
            </P>
            <P>
              <strong>Why GPU first.</strong> v0.1 of the GPU miner
              moves BLAKE2b leaf generation (~70% of solver time) onto
              the GPU; v0.2 moves the rest of Wagner. Both versions
              share one Rust core and one set of WGSL shaders running
              across Metal, Vulkan, and DX12 — no CUDA, no driver
              install, no admin rights. The on-chain protocol is
              committed to (96, 5) forever, so every accelerated
              implementation we ship is open-source on the same terms
              as the protocol itself.
            </P>
          </Section>

          <div className="rounded-2xl border border-[var(--color-border)] bg-[var(--color-bg-elev)] p-5 md:p-6 mt-12">
            <h3 className="text-[16px] font-bold tracking-[-0.01em] mb-2">
              Stuck?
            </h3>
            <p className="text-[14px] leading-[1.6] text-[var(--color-fg-dim)]">
              The full source is at{" "}
              <a
                href="https://github.com/HannaPrints/equium"
                target="_blank"
                rel="noreferrer noopener"
                className="text-[var(--color-rose)] hover:underline"
              >
                github.com/HannaPrints/equium
              </a>
              . Open an issue if you hit something specific or post in
              the X replies on{" "}
              <a
                href="https://x.com/EquiumEQM"
                target="_blank"
                rel="noreferrer noopener"
                className="text-[var(--color-rose)] hover:underline"
              >
                @EquiumEQM
              </a>
              .
            </p>
          </div>
        </div>
      </div>
      <Footer />
    </main>
  );
}

function OsCard({
  label,
  hint,
  href,
}: {
  label: string;
  hint: string;
  href: string;
}) {
  return (
    <Link
      href={href}
      className="rounded-2xl border border-[var(--color-border)] bg-[var(--color-bg-elev)] p-5 hover:border-[var(--color-rose-soft)] transition-colors group"
    >
      <div className="text-[18px] font-bold tracking-[-0.01em] group-hover:text-[var(--color-rose)] transition-colors">
        {label}
      </div>
      <div className="text-[12px] font-mono text-[var(--color-fg-dim)] mt-1">
        {hint}
      </div>
    </Link>
  );
}

function Section({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section id={id} className="scroll-mt-32 mb-14">
      <h2 className="text-[26px] font-bold tracking-[-0.018em] mb-4">
        {title}
      </h2>
      <div className="space-y-4">{children}</div>
    </section>
  );
}

function Block({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="text-[10px] font-mono uppercase tracking-[0.18em] text-[var(--color-fg-dim)] mb-2 font-semibold">
        {label}
      </div>
      {children}
    </div>
  );
}

function P({ children }: { children: React.ReactNode }) {
  return (
    <p className="text-[14.5px] leading-[1.65] text-[var(--color-fg-soft)]">
      {children}
    </p>
  );
}

function Pre({ children }: { children: React.ReactNode }) {
  return (
    <pre className="rounded-xl border border-[var(--color-border)] bg-[var(--color-bg)] p-4 overflow-x-auto font-mono text-[12.5px] leading-[1.7] text-[var(--color-fg-soft)]">
      {children}
    </pre>
  );
}

function Code({ children }: { children: React.ReactNode }) {
  return (
    <code className="font-mono text-[12.5px] px-1.5 py-0.5 rounded bg-[var(--color-bg)] border border-[var(--color-border)] text-[var(--color-teal)]">
      {children}
    </code>
  );
}

function Callout({
  children,
  tone = "default",
}: {
  children: React.ReactNode;
  tone?: "default" | "dim";
}) {
  const cls =
    tone === "dim"
      ? "border-[var(--color-border)] bg-[var(--color-bg-elev)] text-[var(--color-fg-dim)]"
      : "border-[var(--color-gold)]/40 bg-[var(--color-gold)]/[0.06]";
  return (
    <div
      className={`rounded-2xl border p-5 my-2 text-[14px] leading-[1.6] ${cls}`}
    >
      {children}
    </div>
  );
}
