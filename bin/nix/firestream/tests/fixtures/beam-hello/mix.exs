defmodule BeamHello.MixProject do
  use Mix.Project

  def project do
    [
      app: :beam_hello,
      version: "0.1.0",
      elixir: "~> 1.18",
      start_permanent: true,
      deps: [],
      releases: [beam_hello: [include_executables_for: [:unix]]]
    ]
  end

  def application do
    [extra_applications: [:logger], mod: {BeamHello.Application, []}]
  end
end
