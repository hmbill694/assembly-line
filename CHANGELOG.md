# Changelog

## 0.1.0 (2026-09-28)


### ⚠ BREAKING CHANGES

* **job:** status reports a job whose last round passed as "passed", not "succeeded".
* **event:** event lines written before this change are skipped as unreadable, so an older job reads as pending and a revise of it restarts at round 1 in its append-only log. Delete .assembly/jobs from earlier versions before running this one.

### Features

* **cli:** assembly review ([8ef36ae](https://github.com/hmbill694/assembly-line/commit/8ef36ae120afa32debcc59f91373174b43bf3bc9))
* **cli:** gc for worktrees left behind by failed nodes ([4288c3c](https://github.com/hmbill694/assembly-line/commit/4288c3c72fa7896df17b3076297e52b144526b74))
* **cli:** validate, run, resume, status, and logs commands ([81844ea](https://github.com/hmbill694/assembly-line/commit/81844ea47c663ec0da22db677a5932286078094f))
* **config:** task graph model, TOML parsing, and prompt_file support ([889aa72](https://github.com/hmbill694/assembly-line/commit/889aa72d906f5c815a6ebfdbcc66cecfa7bbb7e8))
* **dag:** graph validation reporting every problem at once ([d5196a3](https://github.com/hmbill694/assembly-line/commit/d5196a3c587bb2ff8d0e2ded2fb9d53fda294fb4))
* **delivery:** a verified job opens a pull request ([625e2f9](https://github.com/hmbill694/assembly-line/commit/625e2f96218a81c140320444d2215e6b19bd70f8))
* **delivery:** open a pull request, or push onto the base ([e7c7ad8](https://github.com/hmbill694/assembly-line/commit/e7c7ad83e260a9f50dcef4fc462ce4ea66835df7))
* **event:** append-only event log ([5383bdf](https://github.com/hmbill694/assembly-line/commit/5383bdf3fb36353576e25fb49f77e3a08684bfa6))
* **event:** record commits, merges, and merge conflicts ([5694793](https://github.com/hmbill694/assembly-line/commit/5694793ad90681a9942f701fabde0bd7c77dc981))
* **event:** review is a second axis, orthogonal to execution ([ee7a2b9](https://github.com/hmbill694/assembly-line/commit/ee7a2b98f48b062857feca0729d2944458a63427))
* **exec:** run a program with an explicit argument vector ([58d2c2c](https://github.com/hmbill694/assembly-line/commit/58d2c2c1e192e8da40ad112725fd4ea2f4c6e48e))
* **exec:** shell execution with log capture, timeout, and cancellation ([615ddcc](https://github.com/hmbill694/assembly-line/commit/615ddcc8ec51b89aef846e8fa652eb6dc5083a19))
* f2 implementation ([#8](https://github.com/hmbill694/assembly-line/issues/8)) ([3f41647](https://github.com/hmbill694/assembly-line/commit/3f41647722b71a639348969ced43c2998b1d94de))
* **git:** check out an existing branch, and delete a superseded one ([f1b87fc](https://github.com/hmbill694/assembly-line/commit/f1b87fcf9a5e8aa74e13b598442fd783f912805a))
* **git:** publish a branch to a remote ([cb618f8](https://github.com/hmbill694/assembly-line/commit/cb618f8c387c9275b494891cb613c59098595291))
* **git:** worktree, commit, and merge operations for run branches ([22670ec](https://github.com/hmbill694/assembly-line/commit/22670ecf4a3cb2cae3a50699d64b4454533cf4aa))
* **job:** a job is a repo, a ref and a prompt ([fe9f5e9](https://github.com/hmbill694/assembly-line/commit/fe9f5e91c8933c64914fdef100d9739b57327f12))
* **job:** verify decides whether a job succeeded ([5be5793](https://github.com/hmbill694/assembly-line/commit/5be579387c2346aa4d6781dbb3e96c508f665ecd))
* **paths:** repo-scoped worktree locations and run-branch metadata ([3c81449](https://github.com/hmbill694/assembly-line/commit/3c814495e145dc3c8e4f0846693bc7931f92705f))
* **paths:** run directory layout and git root discovery ([bc9d8e8](https://github.com/hmbill694/assembly-line/commit/bc9d8e873def23f290e2241511eb0ab8ee5029bc))
* **provider:** render a provider command with the node's prompt ([43dde7b](https://github.com/hmbill694/assembly-line/commit/43dde7be2bc457ef0657453cc334b80d53b65c58))
* **report:** per-node run summary folded from events ([4472d25](https://github.com/hmbill694/assembly-line/commit/4472d25b6335ad456cabcee93e0413cd16bd9e70))
* **review:** the review inbox as a fold over the event log ([24e3dcc](https://github.com/hmbill694/assembly-line/commit/24e3dcc4373b7c3bea353734f970cd282d147208))
* **scheduler:** a node's branch survives, its worktree does not ([40b90c3](https://github.com/hmbill694/assembly-line/commit/40b90c32bc843325da0a9c35eb79cc535f7451ed))
* **scheduler:** parallel execution with job cap, resources, and failure semantics ([6c317d1](https://github.com/hmbill694/assembly-line/commit/6c317d10d1f69d3eadbbdc8a3a2d680f0fffbfa1))
* **scheduler:** revise is a new job seeded from the node's branch ([7361161](https://github.com/hmbill694/assembly-line/commit/73611614b2da418c4d9ab030eb3290437dace5fb))
* **scheduler:** run agent nodes in worktrees and merge into the run branch ([f6a871a](https://github.com/hmbill694/assembly-line/commit/f6a871a6c52fa78608f1476c40ceba7929bb958b))
* **state:** run state as a pure fold over the event log ([cf21667](https://github.com/hmbill694/assembly-line/commit/cf21667b239cf0601df4996f0c64dce50b49b342))
* **workspace:** per-node worktrees, seeded and superseding a prior attempt ([26d9346](https://github.com/hmbill694/assembly-line/commit/26d9346774b952348dc001c75984c69d9bd48df2))


### Bug Fixes

* address final review findings (CLAUDE.md doc drift, config naming, comment accuracy, test teeth) ([c0d7c95](https://github.com/hmbill694/assembly-line/commit/c0d7c9598a10e4680c226e2a0dd59cccfbb2e79d))
* **cli:** a revise round reports the node, not the run ([27b5454](https://github.com/hmbill694/assembly-line/commit/27b5454b31b0872bd6697f961e512285f1495a6e))
* delete what nothing reads ([0abd009](https://github.com/hmbill694/assembly-line/commit/0abd0094326d1dc4162a4475848f959f40d63ec2))
* **deps:** update rust crate toml to 0.9 ([8c24da1](https://github.com/hmbill694/assembly-line/commit/8c24da181e32987f8c7bc317e3d3fe89764de143))
* **deps:** update rust crate toml to v1 ([#6](https://github.com/hmbill694/assembly-line/issues/6)) ([91c9919](https://github.com/hmbill694/assembly-line/commit/91c9919b6e37633b0c6546918a2c1245b007cba4))
* gitignore job state and wire config.base to its pull-request target ([f288101](https://github.com/hmbill694/assembly-line/commit/f28810168fc8654b1038d236056c87a73682aa80))


### Refactors

* centralize folding logic ([#18](https://github.com/hmbill694/assembly-line/issues/18)) ([db39f54](https://github.com/hmbill694/assembly-line/commit/db39f5477ad5996ddec2ce910fea947a432d0bf6))
* **cli:** a pull request's base is read once ([f0710ab](https://github.com/hmbill694/assembly-line/commit/f0710ab47e9f1b4f4191897d994f1be7fbbb9bbd))
* **cli:** every async command reports its usage error the same way ([58413e2](https://github.com/hmbill694/assembly-line/commit/58413e2236b674896ea051aa2867cc793574bfbd))
* **cli:** the config-and-provider check names what it decides ([ae0a008](https://github.com/hmbill694/assembly-line/commit/ae0a008a81e3d6b0302e62cbe3f9cea854a18af3))
* **cli:** the runner flags know which runners they apply to ([bd885e6](https://github.com/hmbill694/assembly-line/commit/bd885e6b67e13fa0bcd5be3a7eb0aec736f0133f))
* **dag:** tasks no longer depend on each other ([8a6c09b](https://github.com/hmbill694/assembly-line/commit/8a6c09bc7e2b62b71687858fb58673650e66e326))
* **event:** events name the round ([fa88dbe](https://github.com/hmbill694/assembly-line/commit/fa88dbed3f544565df2b77071e03e3c79a3c28cc))
* **gc:** deleting one repository's leftovers is its own value ([39002cc](https://github.com/hmbill694/assembly-line/commit/39002cc15604898ab98a6cf1104920a25ab6b7f6))
* **gc:** scratch worktrees need no policy in the CLI ([efb5a07](https://github.com/hmbill694/assembly-line/commit/efb5a072f3fb449dbb96357c55fda3e3615231d5))
* **git:** name the reason a worktree's parent is created ([7bc9a4b](https://github.com/hmbill694/assembly-line/commit/7bc9a4b9bb7a779ebaed32cb5f95b3ebefbd2be6))
* **git:** where a worktree starts is one enum, not four shapes ([c9ffd69](https://github.com/hmbill694/assembly-line/commit/c9ffd695eac897ebcf7215f906e9bae7dbc51bea))
* help, messages and docs say round where they mean one ([843c876](https://github.com/hmbill694/assembly-line/commit/843c876e2d5e5e11c2f5c44d9d95765e4668d79c))
* **job:** a job's outcome is an enum, not a bool ([e6808ef](https://github.com/hmbill694/assembly-line/commit/e6808ef0b28558c7c23d101d00fd06693052b18a))
* **job:** a round ends in a verdict ([4c3bf38](https://github.com/hmbill694/assembly-line/commit/4c3bf385b4a74b4a70df0522366f3b53f712b402))
* **job:** resolving a start commit is not where a repository is readied ([8c3d871](https://github.com/hmbill694/assembly-line/commit/8c3d871407025c7cb63b9902e40e979a215d6b67))
* **job:** the module that runs one job is not a scheduler ([143d89b](https://github.com/hmbill694/assembly-line/commit/143d89b0a3797dfc946d0950290f9589367f2d04))
* **job:** why the agent failed is an Option, not a RoundResult ([9655f82](https://github.com/hmbill694/assembly-line/commit/9655f824b54c5807b19d50a1631c653a959899aa))
* **lifecycle:** logs finds its output in the library ([2aafd45](https://github.com/hmbill694/assembly-line/commit/2aafd452b2a4bb2951af00f1bbd734d3d83af80c))
* **lifecycle:** starting and revising a job is a library concern ([5801fa2](https://github.com/hmbill694/assembly-line/commit/5801fa28674aa38a01ac8c63c3a679e8a1facf61))
* **lifecycle:** status finds its job in the library ([ebe70af](https://github.com/hmbill694/assembly-line/commit/ebe70af5264afb6afdcf7a9d6a4ea82284e097ed))
* make job ownership distinct and decouple from workspace ([#17](https://github.com/hmbill694/assembly-line/issues/17)) ([a25d052](https://github.com/hmbill694/assembly-line/commit/a25d052c833a05d5cd6c03663cc6d6b19963b934))
* move container logic to runner ([#19](https://github.com/hmbill694/assembly-line/issues/19)) ([9222d8b](https://github.com/hmbill694/assembly-line/commit/9222d8b6ff83019a42822cb965b3aa59f0aa4a77))
* **payload:** a payload is one round's instructions ([81aeef3](https://github.com/hmbill694/assembly-line/commit/81aeef36b83757a605ee102616a517f626a56053))
* **payload:** a payload reads itself from the environment ([8bf9951](https://github.com/hmbill694/assembly-line/commit/8bf9951f7e3a4f8642800f7b1033c929cc42a04b))
* reorganizing the codebase ([c65cc32](https://github.com/hmbill694/assembly-line/commit/c65cc3281d323d7c9b6af5f278decb56c04ccfe2))
* **report:** an attempt is a round ([7b8f11f](https://github.com/hmbill694/assembly-line/commit/7b8f11fbf5807c112942c0d861fb0527faf6271b))
* **review:** the pull request is the inbox ([a993e80](https://github.com/hmbill694/assembly-line/commit/a993e80d37334a656dc4fc8c7aecf1522e9df60b))
* **round:** the module that runs a round is named for it ([add4515](https://github.com/hmbill694/assembly-line/commit/add451576efb3e1eeef9dc90bbec5e1164dbd60c))
* **runner:** a runner runs a round ([3f16371](https://github.com/hmbill694/assembly-line/commit/3f163715475616a1a2a6b623e4d28576c3eecf21))
* **scheduler:** a job's branch is cut from its base ref ([67bcc17](https://github.com/hmbill694/assembly-line/commit/67bcc177fc348e2ed332f4c4402bc97934b215ba))
* **scheduler:** a round's ordering is in its shape, not a comment ([df25879](https://github.com/hmbill694/assembly-line/commit/df25879ef09f8d22153f36674cc740b04b70df8e))
* **scheduler:** every task is an agent ([612612b](https://github.com/hmbill694/assembly-line/commit/612612ba014f899d91e79bea3db6652d5f04b0b1))
* **state:** folding an event returns the next state ([0b2c849](https://github.com/hmbill694/assembly-line/commit/0b2c8494787ccdcf64688201aa613be2636740b3))
* **workspace:** a workspace holds one round's checkout ([576bbd9](https://github.com/hmbill694/assembly-line/commit/576bbd941a3a7c6d9ddcfd8a14653e07a5cbecc8))
* **workspace:** creating a checkout names its three steps ([46b699b](https://github.com/hmbill694/assembly-line/commit/46b699b0556b3cf1a2e5105e58715edd0d339f5d))


### Documentation

* assembly-line design spec and M1 checklist ([566db45](https://github.com/hmbill694/assembly-line/commit/566db45d8d30959c0eee2e2b755ab7495e13c0da))
* contributing says how a change lands and how releases are cut ([7378780](https://github.com/hmbill694/assembly-line/commit/73787806241564fb02e38f848b6d38dec01384ae))
* F1 shipped ([e56d25a](https://github.com/hmbill694/assembly-line/commit/e56d25a0623a109aa88cf43180c63e09d19d224b))
* M2 implementation plan for agent nodes in worktrees ([2a01869](https://github.com/hmbill694/assembly-line/commit/2a0186954082c91ee45d241e2c30c35bf0449b6c))
* M3 implementation plan for stateless jobs ([95f1a1b](https://github.com/hmbill694/assembly-line/commit/95f1a1b61ed6ec3c9dea94e98d4d31dbacafe327))
* M3 spec update — the job contract ([83fdeac](https://github.com/hmbill694/assembly-line/commit/83fdeac687d7f7a2ea6bda56aba6602d1b1edf78))
* one owner per fact ([5d81ea1](https://github.com/hmbill694/assembly-line/commit/5d81ea1ab99b85930e0c0911e5fefbb42cbcf69f))
* project conventions for functional style, naming, and abstraction ([68294f7](https://github.com/hmbill694/assembly-line/commit/68294f75bf187bf3ca34edb807ac633776463117))
* record M2 scope, limitations, and accepted risks ([b6791e9](https://github.com/hmbill694/assembly-line/commit/b6791e9469d318efa1f9be87ba7c6db6a7128d64))
* record the jj stacked, always-compilable change workflow ([aefbd37](https://github.com/hmbill694/assembly-line/commit/aefbd3720ef76c3d090dcb5b0bc1bf17d56f6871))
* separate run-level, agent-level, and workspace isolation concerns ([61f3c7e](https://github.com/hmbill694/assembly-line/commit/61f3c7e903caf7201f15e07fef3a5f9d0ebb4f82))


### Build System

* cargo scaffold on edition 2024, pinned to rust 1.97.1 ([eddd6d1](https://github.com/hmbill694/assembly-line/commit/eddd6d19e5bb4ee4f045f653cd3a69fcf1d945a6))
* **devenv:** the shell reads rust-toolchain.toml and carries cargo-audit ([9d17f40](https://github.com/hmbill694/assembly-line/commit/9d17f4028dc4ea2f3a44a5a73f20ad5fa5583de2))
* enforce clippy pedantic and document error contracts ([3334101](https://github.com/hmbill694/assembly-line/commit/3334101fc4e568e556c7a4ba620a5a0b66b23631))
* **just:** ci is the list CI runs, audit scans the lockfile ([80068b0](https://github.com/hmbill694/assembly-line/commit/80068b01807616d475b4dd0f8d5d6a534d1a5bdc))
* justfile and devenv shell pinned to the project toolchain ([9d06cf6](https://github.com/hmbill694/assembly-line/commit/9d06cf6465e47e7ff34f444828b065c41bc8dc90))
* verify-stack cleans up its own build artifacts ([5e133d9](https://github.com/hmbill694/assembly-line/commit/5e133d974d8b5f0eed02ddc9c2ecca1c5f2d9f99))


### Chores

* de-ai the codebase ([360fb92](https://github.com/hmbill694/assembly-line/commit/360fb92221b41204132da8dc1dc6d44726f388c2))
* **deps:** pin dependencies ([#20](https://github.com/hmbill694/assembly-line/issues/20)) ([239fa17](https://github.com/hmbill694/assembly-line/commit/239fa17a01b10a376bb229e819b922ae8e8e0b35))
* **deps:** update dependency jdx/mise to v2026.9.15 ([#9](https://github.com/hmbill694/assembly-line/issues/9)) ([7388ac5](https://github.com/hmbill694/assembly-line/commit/7388ac55ebd95d0622af8c20bf707a261951755c))
* **deps:** update dev dependencies ([#21](https://github.com/hmbill694/assembly-line/issues/21)) ([8ba6910](https://github.com/hmbill694/assembly-line/commit/8ba6910932e3795684a605db68f5b729f37a1c99))
* **deps:** update devenv inputs ([#26](https://github.com/hmbill694/assembly-line/issues/26)) ([23202c3](https://github.com/hmbill694/assembly-line/commit/23202c383e1bd11bbf3fa944082181263674133d))
* **deps:** update rust crate chrono to 0.4.45 ([#23](https://github.com/hmbill694/assembly-line/issues/23)) ([491a601](https://github.com/hmbill694/assembly-line/commit/491a601cd451c30d66fc8eeaf2c1b86ea6418902))
* **deps:** update rust crate clap to v4.6.7 ([68b425e](https://github.com/hmbill694/assembly-line/commit/68b425ea3487571f20dd8d135b073733deded695))
* **deps:** update rust crate humantime to 2.4.0 ([#25](https://github.com/hmbill694/assembly-line/issues/25)) ([b7260cf](https://github.com/hmbill694/assembly-line/commit/b7260cf0322ea3a55d6855d427955fbe2dba9de0))
* **deps:** update rust crate nix to 0.31.3 ([#10](https://github.com/hmbill694/assembly-line/issues/10)) ([d160f6f](https://github.com/hmbill694/assembly-line/commit/d160f6fe7dc3eedd02b08f7cc71973ff1e46a787))
* **deps:** update rust to v1.98.1 ([988412f](https://github.com/hmbill694/assembly-line/commit/988412fe6910430f8c48e2592ed421c96aebe457))
* F1 spec and plan ([04be23d](https://github.com/hmbill694/assembly-line/commit/04be23d201ac0957bddf4b0b18eef65f5952e8df))
* get ready for manual testing ([52496bd](https://github.com/hmbill694/assembly-line/commit/52496bd8163aa70f5a8f6a8993e2c239af1eab8f))
* quality and review sweep ([65c2b71](https://github.com/hmbill694/assembly-line/commit/65c2b71da51785da5e427a4fcd3d1b79dbaebf46))
* **renovate:** conventional titles, pinned action digests, a weekly batch ([84b0f84](https://github.com/hmbill694/assembly-line/commit/84b0f8449e368dc68c69d7746a3b3bd5d3402d35))
* testing the thing ([aa31f57](https://github.com/hmbill694/assembly-line/commit/aa31f5757be0f1dbc04c2a8b151a474ecd0d0216))
