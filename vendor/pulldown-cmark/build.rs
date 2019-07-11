
fn main() {
    generate_tests_from_spec()
}

// If the "gen-tests" feature is absent,
// this function will be compiled down to nothing
#[cfg(not(feature="gen-tests"))]
fn generate_tests_from_spec() {}

// If the feature is present, generate tests
// from any .txt file present in the specs/ directory
//
// Test cases are present in the files in the
// following format:
//
// ```````````````````````````````` example
// markdown
// .
// expected html output
// ````````````````````````````````
#[cfg(feature="gen-tests")]
fn generate_tests_from_spec() {
    use std::path::{PathBuf};
    use std::fs::{self, File};
    use std::io::{Read, Write};

    // This is a hardcoded path to the CommonMark spec because it is not situated in
    // the specs/ directory. It's in an array to easily chain it to the other iterator
    // and make it easy to eventually add other hardcoded paths in the future if needed
    let hardcoded = ["./third_party/CommonMark/spec.txt"];
    let hardcoded_iter = hardcoded.into_iter()
                                  .map(PathBuf::from);

    // Create an iterator over the files in the specs/ directory that have a .txt extension
    let spec_files = fs::read_dir("./specs")
                        .expect("Could not find the 'specs' directory")
                        .filter_map(Result::ok)
                        .map(|d| d.path())
                        .filter(|p| p.extension().map(|e| e.to_owned()).is_some())
                        .chain(hardcoded_iter);

    for file_path in spec_files {
        let mut raw_spec = String::new();

        File::open(&file_path)
             .and_then(|mut f| f.read_to_string(&mut raw_spec))
             .expect("Could not read the spec file");

        let rs_test_file = PathBuf::from("./tests/")
                                   .join(file_path.file_name().expect("Invalid filename"))
                                   .with_extension("rs");

        let mut spec_rs = File::create(&rs_test_file)
                               .expect(&format!("Could not create {:?}", rs_test_file));

        let spec_name = file_path.file_stem().unwrap().to_str().unwrap();

        let spec = Spec::new(&raw_spec);
        let mut n_tests = 0;

        spec_rs.write(b"// This file is auto-generated by the build script\n").unwrap();
        spec_rs.write(b"// Please, do not modify it manually\n").unwrap();
        spec_rs.write(b"\nextern crate pulldown_cmark;\n").unwrap();

        for (i, testcase) in spec.enumerate() {
            spec_rs.write_fmt(
                format_args!(
                    r###"

    #[test]
    fn {}_test_{i}() {{
        let original = r##"{original}"##;
        let expected = r##"{expected}"##;

        use pulldown_cmark::{{Parser, html, Options}};

        let mut s = String::new();

        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_TABLES);
        opts.insert(Options::ENABLE_FOOTNOTES);

        let p = Parser::new_ext(&original, opts);
        html::push_html(&mut s, p);

        assert_eq!(expected, s);
    }}"###,
                    spec_name,
                    i=i+1,
                    original=testcase.original,
                    expected=testcase.expected
                ),
            ).unwrap();

            n_tests += 1;
        }

        println!("cargo:warning=Generated {} tests in {:?}", n_tests, rs_test_file);
    }
}

#[cfg(feature="gen-tests")]
pub struct Spec<'a> {
    spec: &'a str,
}

#[cfg(feature="gen-tests")]
impl<'a> Spec<'a> {
    pub fn new(spec: &'a str) -> Self {
        Spec{ spec: spec }
    }
}

#[cfg(feature="gen-tests")]
pub struct TestCase {
    pub original: String,
    pub expected: String,
}

#[cfg(feature="gen-tests")]
impl<'a> Iterator for Spec<'a> {
    type Item = TestCase;

    fn next(&mut self) -> Option<TestCase> {
        let spec = self.spec;

        let i_start = match self.spec.find("```````````````````````````````` example\n").map(|pos| pos + 41) {
            Some(pos) => pos,
            None => return None,
        };

        let i_end = match self.spec[i_start..].find("\n.\n").map(|pos| (pos + 1) + i_start){
            Some(pos) => pos,
            None => return None,
        };

        let e_end = match self.spec[i_end + 2..].find("````````````````````````````````\n").map(|pos| pos + i_end + 2){
            Some(pos) => pos,
            None => return None,
        };

        self.spec = &self.spec[e_end + 33 ..];

        let test_case = TestCase {
            original: spec[i_start .. i_end].to_string().replace("→", "\t"),
            expected: spec[i_end + 2 .. e_end].to_string().replace("→", "\t")
        };

        Some(test_case)
    }
}
