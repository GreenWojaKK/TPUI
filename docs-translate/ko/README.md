# TPUI

[English](../../README.md) | 한국어

TPUI는 코딩 에이전트, 터미널 세션, 작업 중인 파일을 함께 다루는 작업 환경을 위해 개발하는 데스크톱 UI 기반이며, [GPUI](https://github.com/GreenWojaKK/zed) fork를 사용한다. Rust/GPUI가 화면을, Go가 에이전트 코어를 맡고 tmux로 터미널 세션을 관리하는 구성을 목표로 한다.

여러 에이전트와 개발 도구를 동시에 실행하는 환경에서 UI가 더하는 메모리 부담을 작게 유지하려 한다.

함께 제공하는 `viewer-app`은 이 기술 스택을 처음 적용하는 앱이다. 이 기반으로 소스 코드, 문서, 구조화 데이터를 탐색한다.

## UI 컴포넌트

| 영역 | 컴포넌트 |
|---|---|
| 텍스트 | 줄바꿈과 스타일을 지원하는 텍스트, 선택 |
| 배치와 탐색 | 표, 탭, 메뉴, 버튼 |
| 시각화 | 선·밴드 차트, 막대·스트립 차트, 히트맵, 경로 지도 |
| 앱 연동 | 밝은·어두운 테마, Windows 네이티브 입력 창, 메시지 파일 inbox |

앱이 데이터를 전달하고 컴포넌트 이벤트를 처리한다. 구성별 책임은 [아키텍처](architecture.md#공용-ui-컴포넌트)를 참조한다.

## 앱에서 `viewer-ui` 사용하기

Git 의존성으로 추가하고 GPUI를 이 저장소와 동일한 fork commit으로 고정한다. 다른 commit을 사용하면 GPUI가 두 번 빌드되고 타입이 일치하지 않는다.

```toml
[dependencies]
viewer-ui = { git = "https://github.com/GreenWojaKK/TPUI", rev = "<TPUI commit>" }
gpui = { git = "https://github.com/GreenWojaKK/zed", rev = "f941f6a1c26a295c051e2a3776d9897093b3db8c" }
```

기본 테마에서 필요한 값을 바꾼 뒤 첫 창을 열기 전에 설치한다.

```rust
let mut theme = viewer_ui::theme::Theme::default();
theme.light.text = gpui::rgb(0x345575);
theme.metrics.list_row_height = gpui::px(32.);
viewer_ui::theme::install(theme);
```

## GPUI fork

TPUI는 Zed v1.26.2를 기반으로 실행 파일 크기를 줄이도록 이미지 codec을 선택한 [GreenWojaKK/zed](https://github.com/GreenWojaKK/zed)를 사용한다. 위 의존성 예제는 필요한 commit을 고정한다. Fork 변경은 [FORK.md](https://github.com/GreenWojaKK/zed/blob/tpui/v1.26.2-slim-image/FORK.md)에 기록한다.

## 저장소 구성

| 위치 | 역할 |
|---|---|
| `crates/viewer-ui` | 공용 GPUI 컴포넌트와 테마 |
| `crates/viewer-core` | 파일 리더, 문서 모델, 테이블 질의 |
| `crates/viewer-app` | Viewer 앱과 PDF worker |
| `core` | 소스 코드 토큰 서비스 |

## Viewer 앱

`viewer-app`은 TPUI를 사용해 탐색기와 탭으로 파일을 탐색한다.

| 파일 | 화면과 조작 |
|---|---|
| 소스 코드와 설정 파일 | 줄 번호, 텍스트 선택과 복사. Python, Go, Rust, JavaScript, TypeScript 구문 색상 |
| Markdown | 서식 있는 본문, 표, 코드 블록, 로컬 이미지, 링크, 텍스트 선택 |
| JSON과 JSON Lines | 펼칠 수 있는 트리, 값의 경로와 전체 값. 키 순서·중복 키·숫자 표기 보존 |
| Parquet과 SQLite 테이블·뷰 | 데이터 그리드, 스키마, 전체 값, 찾기, 타입별 필터, 단일 컬럼 정렬, 저장 구조 |
| PDF | Windows에서 렌더링한 페이지를 뷰 너비에 맞춰 표시 |

조작, 질의, 편집기 연동은 [Viewer 사용 안내](usage.md)를 참조한다.

### 빌드와 실행

Windows x64 빌드에는 Visual Studio Build Tools 2022(MSVC), Windows SDK와 [rust-toolchain.toml](../../rust-toolchain.toml)의 toolchain이 필요하다. Go 1.24 이상으로 소스 코드 토큰 서비스를 빌드한다. Go가 없으면 소스 파일은 일반 텍스트로 표시한다.

저장소 루트에서 실행한다.

```powershell
cargo build --release -p viewer-app
.\target\release\viewer-app.exe
```

Cargo는 `viewer-app.exe`와 `tpui-pdf.exe`를 빌드한다. Go가 있으면 `tpui-core.exe`도 생성한다. Viewer를 옮길 때는 생성된 실행 파일을 함께 옮긴다.

컴포넌트 설계와 viewer 구현은 [아키텍처](architecture.md)에 설명한다.

## 라이선스

이 저장소의 코드는 [Apache License 2.0](../../LICENSE)에 따라 제공된다.

의존성에는 각각의 라이선스가 적용된다. GPUI와 arrow-rs는 Apache License 2.0, rusqlite와 pulldown-cmark는 MIT, `tpui-core`에 들어가는 Go 표준 라이브러리는 BSD이며, 번들된 SQLite는 퍼블릭 도메인이다. 빌드된 실행 파일을 배포할 때는 이 의존성의 라이선스 고지를 포함한다.
