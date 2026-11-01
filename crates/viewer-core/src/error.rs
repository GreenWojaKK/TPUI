use std::error::Error;
use std::io;

use parquet::errors::ParquetError;

/// 엔진 작업의 실패 원인. 사용자에게 보여줄 문구는 앱이 이 구분을 보고 만든다.
#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    /// 파일을 열거나 크기를 확인하지 못했다. 경로 없음과 권한 오류를 포함한다.
    #[error("파일 입출력 오류: {0}")]
    Io(#[from] io::Error),

    /// Footer나 메타데이터가 Parquet 형식이 아니거나, Arrow 스키마로 바꿀 수 없다.
    #[error("Parquet 메타데이터 오류: {0}")]
    InvalidParquet(#[from] ParquetError),

    /// Row Group의 행 수가 음수이거나, 누적 행 수가 `u64` 범위를 넘는다.
    #[error("Row Group 행 수가 올바르지 않습니다")]
    InvalidRowCount,

    /// 요청한 세션이 현재 열린 파일의 세션이 아니다.
    #[error("이전 파일 세션의 요청입니다")]
    StaleSession,

    /// 행 범위나 컬럼 ID가 요청 계약을 어겼다.
    #[error("잘못된 요청: {0}")]
    InvalidRequest(&'static str),

    /// 데이터 페이지의 압축 해제나 디코딩에 실패했다. 미지원 codec을 포함한다.
    #[error("데이터 디코딩 오류: {0}")]
    Decode(Box<dyn Error + Send + Sync>),

    /// 파일을 연 뒤 크기나 수정 시각이 바뀌었다.
    #[error("파일이 열린 뒤 변경되었습니다")]
    FileChanged,

    /// 필터 조건의 값을 컬럼 타입으로 해석할 수 없거나, 컬럼 타입에 쓸 수 없는 연산자다.
    #[error("{0}")]
    InvalidCondition(String),

    /// 정렬할 수 없는 컬럼 타입이거나, 정렬할 행 수가 `u32` 범위를 넘는다.
    #[error("{0}")]
    Unsortable(String),

    /// 취소 상태가 켜져 작업을 멈췄다.
    #[error("작업이 취소되었습니다")]
    Cancelled,
}
