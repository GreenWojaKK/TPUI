# TPUI

로컬 Parquet 파일의 원본 데이터와 파일 구조를 보는 Windows 데스크톱 뷰어와, 다른 GPUI 앱이 함께 쓰는 화면 부품 crate다. Rust, [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), [arrow-rs](https://github.com/apache/arrow-rs)로 만든다.

분석과 집계는 다른 도구에서 하고, 이 뷰어는 원본 값을 잘림 없이 보는 데 집중한다. 설계와 동작 계약은 [architecture.md](architecture.md)에 있다.

## 기능

- 파일을 열 때 메타데이터만 읽고, 화면에 보이는 행·열만 디코딩한다. 디코딩한 블록은 모든 탭이 함께 쓰는 128 MiB 캐시에 둔다.
- 탐색기, 탭, 스키마 트리, 행·열 가상화 그리드
- 값 상세 보기: 긴 문자열, Binary, 중첩 값을 잘림 없이 표시하고 원본 그대로 복사한다.
- 값 전체가 같은 셀 찾기, 컬럼별 필터, 컬럼 정렬
- 파일 구조 보기: 컬럼·Row Group별 크기, 압축, 인코딩, null 수, 최솟값·최댓값

## crate

| crate | 내용 |
|---|---|
| `viewer-core` | Parquet 읽기, 필터·정렬·찾기 계산, 파일 구조. GPUI에 의존하지 않는다 |
| `viewer-ui` | GPUI 화면 부품. 테마, 한 줄 입력, 탭 막대, 표, 버튼, 선·띠 차트, 히트맵 |
| `viewer-app` | Parquet 뷰어 앱 |

## 빌드

Windows x64, Visual Studio Build Tools 2022(MSVC), Windows SDK가 필요하다. Rust toolchain은 `rust-toolchain.toml`이 정한다.

```
cargo build --release -p viewer-app
```

결과는 `target/release/viewer-app.exe`다.

차트 예제는 확률예측 explorer 데이터(`explorer-data.json`)를 읽어 그린다.

```
cargo run -p viewer-ui --example forecast_charts -- <explorer-data.json> [YYYY-MM-DD]
```

## 다른 앱에서 `viewer-ui` 쓰기

git 의존성으로 가져오고, GPUI는 이 저장소와 같은 fork 커밋을 지정한다. 커밋이 다르면 GPUI가 두 벌 빌드되어 타입이 맞지 않는다.

```toml
[dependencies]
viewer-ui = { git = "https://github.com/GreenWojaKK/TPUI", rev = "<TPUI 커밋>" }
gpui = { git = "https://github.com/GreenWojaKK/zed", rev = "707d17a3ebb84de27df28429cb8cb5c351e9c4f7" }
```

## GPUI fork

GPUI는 Zed Industries의 [Zed](https://github.com/zed-industries/zed) 저장소에 있는 UI 프레임워크이며 Apache License 2.0을 따른다. 이 저장소는 GPUI를 원본 대신 fork [`GreenWojaKK/zed`](https://github.com/GreenWojaKK/zed)의 `tpui/v1.26.2-slim-image` 브랜치 커밋 `707d17a3`으로 가져온다.

fork는 Zed 릴리스 v1.26.2(`c4273271`)에 커밋 하나를 더한 것이다. 그 커밋은 Zed 루트 `Cargo.toml`의 `image` crate 기능을 gpui가 쓰는 gif·jpeg·png·webp로 줄여 실행 파일 크기를 줄인다. GPUI 소스 코드는 바꾸지 않는다.

## 라이선스

이 저장소의 코드는 [MIT License](LICENSE)를 따른다.

의존성은 각자의 라이선스를 따른다. GPUI와 arrow-rs는 Apache License 2.0이다. 빌드한 실행 파일을 배포할 때는 이들 의존성의 라이선스 고지를 함께 넣는다.
