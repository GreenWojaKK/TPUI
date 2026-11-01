//! Windows 실행 파일에 아이콘 resource를 넣는다. 탐색기·바로 가기·창·작업 표시줄이 이 아이콘을 쓴다.

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("아이콘 resource 컴파일");
}
