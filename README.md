# bindgen-bridge

## What does this do? 

bindgen-bridge reduces type duplication and headers size by renaming the struct and union types exported by cbindgen.

It renames them with the same name they had in C before being imported with bindgen.

Example:

`bindgen` imports the C type `struct my_struct` into Rust with the name `my_struct` (or `struct_my_struct` when using the `c_naming` option).
`cbindgen` exports this type to C using the exact same name it had in Rust, `my_struct`, instead of its C name `struct my_struct`.
 
This duplicates the definition of the type and requires casts in the C code to convert between the duplicated types.

It also supports aliased/typedefed composite types. 


## When is this useful?

This can be useful when writing Rust code that calls C code and also expects to be called in C. 
For example, when writing a Rust library that extends the features of a C project.


## How does it work?

`bindgen-bridge` uses (for now!) a fork of `bindgen` with extended callbacks capabilities 
that allow it to discover all composite (struct and union) types and their aliases (typedef).

It then reconstructs the original C name of the discovered types and makes a mapping between original type names, Rust type names and their aliases.

It then offers the possibility to extend a `cbindgen.toml` configuration file's `[export.rename]` section with appropriate renaming rules.

This can be done directly, or between crates by generating toml in a literal string or by using `phf_codegen` to generate a static map.


## How can I use it in my project

You can see my [example project](https://github.com/mxyns/pmacct-gauze) which uses the cross-crate variant of the process.

Basically, a bindings crate called `project-bindings` imports the types from C headers using bindgen, and generates the bindings along with the rename mappings
into a `bindings` module.

Then, the library crate called `project-lib` imports the types from the `project-bindings` crate, and generates a `cbindgen.toml` with the correct renaming rules. 

Finally, it the imported bindings to define a C api, and exports their definition with the correct type names using `cbindgen.toml` (by using the `cargo-c` integration).


## Code example

### Generating the rename mappings

In the `build.rs` of your **bindings** crate:
```rs
// A way to store the mappings
let name_mappings = Rc::new(RefCell::new(NameMappings::default()));

// The callback that populates the mappings
let name_mappings_cb = Box::new(NameMappingsCallback(name_mappings.clone()));

// Call bindgen to imports type
let bindings = bindgen::Builder::default()
    // The input header we would like to generate
    // bindings for.
    .header("header.h")
    // Tell cargo to invalidate the built crate whenever any of the
    // included header files changed.
    .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
    
    // Set the mappings callback so that it can listen for type/alias discoveries
    .parse_callbacks(name_mappings_cb)
    
    // Finish the builder and generate the bindings.
    .generate()
    
    // Unwrap the Result and panic on failure.
    .expect("Unable to generate bindings");

// Retrieve the mappings
let mut name_mappings: NameMappings = name_mappings.take();

// Free some spaces if you do not plan on doing something with aliases that didn't get assigned to a type 
name_mappings.forget_unused_aliases();

println!("Discovered mappings = {:#?}", name_mappings);

// Create your bindings module with bindgen as usual
let out_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");

bindings
    .write_to_file(&out_path)
    .expect("Couldn't write bindings!");

// I like to use a feature to enable but this but you don't have to
if cfg!(feature = "export-renames") {
    export_renames(name_mappings, &out_path)?;
}
```

#### Exporting directly as .toml literal string

In the `build.rs` of your **bindings** crate:
```rs
// The boolean value lets you specify if you'd rather use an existing alias (typedefed `my_struct`) or the raw C name (`struct my_struct`)
let toml: String = name_mappings.to_cbindgen_toml_renames(false)?;

let generated_code: TokenStream = quote! {
    pub fn bindings_renames() -> &'static str {
        #toml
    }
}

// Append `generated_code` to your bindings.rs file or into a separate file if you want to
```

#### Exporting as a `phf` static map

In the `build.rs` of your **bindings** crate:
```rs
let map: TokenStream = name_mappings
            .to_static_map(false)
            .unwrap()
            .build()
            .to_string()
            .parse()?;

let generated_code = quote! {
    pub static static_renames: phf::Map<&'static str, &'static str> = #map;
}

// Append `generated_code` to your bindings.rs file or into a separate file if you want to
```

### Using the rename mappings

If you do not want to have your original file overwritten, make a "template" of it.
In the `build.rs` of your **library** crate:
```rs
// Load your template file
// You can also provide a wrong file path (it's always used in the header text)
// And force the use of your already parsed toml_edit::Document by using `Template::use_document`
let template: Template = Template::load_template("cbindgen.toml.template")
    .read_as_toml()?
    .with_bindings(&project_bindings::static_renamed);
    
// Generate the bytes for the config file header which informs users that the file has been generated automatically
// based on the template specified in the file path argument of `Template::load_template` 
let header = template.config_header()?.as_bytes();

// Generate the bytes for the config file.
// This is the same document as provided to the template but with the [export.rename] section **extended** (not overwritten!)
// with the rename rules, be careful to introduce duplicate entries as those will, however, be overwritten. 
let config = template.generate_toml()?.to_string().as_bytes();

// Open the final `cbindgen.toml` config file and write the generated content to it
// Warning: this is generated directly in the project root directory, and in the $OUT_DIR, as I can't make it work like this
//          so be careful, you will overwrite your original file.   
let out_file = PathBuf::from("cbindgen.toml");
let mut file = File::create(&out_file)?;
file.write_all(header)?;
file.write_all(config)?;
```

### Using in the same crate

You should be able to avoid writing/loading the bindings to/from a file by if you do everything in the same crate. 
Though I have not tried it yet. 