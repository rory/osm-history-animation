extern crate osmio;
extern crate image;
extern crate gif;
extern crate clap;
extern crate orthoproj;

use clap::{Arg, App, AppSettings};

use std::fs;
use osmio::OSMReader;
use osmio::pbf::PBFReader;
use std::io::{Read, Write, BufReader, BufRead, BufWriter};
use std::collections::HashMap;
use std::cmp::Ordering;

use image::ImageBuffer;

use gif::SetParameter;

// (frame_no, (pixel_index, num_changes))
type Frames = Vec<(u32, Vec<(u32, u16)>)>;

type Metadata = HashMap<String, String>;

struct ColourRamp {
    empty_colour: (u8, u8, u8),
    steps: Vec<(u32, (u8, u8, u8))>,
}

impl ColourRamp {
    fn new_from_filename(filename: &str) -> Self {
        let mut contents = String::new();
        let mut file = fs::File::open(filename).unwrap();
        file.read_to_string(&mut contents).expect("Couldn't read colour ramp file");
        Self::new_from_text(&contents)

    }
    fn new_from_text(source: &str) -> Self {
        let lines: Vec<_> = source.lines().collect();
        let empty_vec = lines[0].split(",").filter_map(|x| x.parse::<u8>().ok()).take(3).collect::<Vec<_>>();
        let empty = (empty_vec[0], empty_vec[1], empty_vec[2]);

        let mut steps = Vec::new();
        for line in lines.iter().skip(1) {
            let line = line.split(",").filter_map(|x| x.parse::<u32>().ok()).take(4).collect::<Vec<_>>();
            let age = line[0];
            let colour = (line[1] as u8, line[2] as u8, line[3] as u8);
            steps.push((age, colour));
        }

        if steps.len() > 254 {
            panic!("Too many steps");
        }

        ColourRamp{ empty_colour: empty, steps: steps }
    }

    fn palette(&self) -> Vec<u8> {
        let mut results = Vec::with_capacity((self.steps.len()+1)*3);
        results.push(self.empty_colour.0);
        results.push(self.empty_colour.1);
        results.push(self.empty_colour.2);

        for &(_, (r, g, b)) in self.steps.iter() {
            results.push(r);
            results.push(g);
            results.push(b);
        }

        results
    }

    fn index_for_magnitude(&self, magnitude: Option<u32>) -> u8 {
        match magnitude {
            None => 0,
            Some(magnitude) => {
                if magnitude > 255 {
                    1
                } else {
                    (255 - magnitude) as u8
                }
            },
        }
    }
}

fn read_pbf(filename: &str, sec_per_frame: u32, pixel_func: Box<Fn(f32, f32) -> Option<u32>>) -> Frames {
    let file = BufReader::new(fs::File::open(&filename).unwrap());
    let mut node_reader = PBFReader::new(file);
    let node_reader = node_reader.nodes();
    
    let mut results: HashMap<u32, HashMap<u32, u16>> = HashMap::new();

    // 1st April 2005, midnight GMT. We presume no OSM editing before then.
    let osm_epoch = 1109635200;

    let mut first_frame_no = std::u32::MAX;
    let mut last_frame_no = 0;

    let mut num_nodes: u64 = 0;
    for node in node_reader {
        if let (Some(lat), Some(lon)) = (node.lat, node.lon) {

            let timestamp = node.timestamp.to_epoch_number() as u64;
            if timestamp < osm_epoch {
                panic!("timestamp before epoch. Change code. {}", timestamp);
            }
            let offset = (timestamp - osm_epoch) as u32;
            // TODO double check that this rounds down
            let frame_no = offset / sec_per_frame;

            if frame_no < first_frame_no {
                first_frame_no = frame_no;
            }
            if frame_no > last_frame_no {
                last_frame_no = frame_no;
            }

            //let pixel_idx = latlon_to_pixel_index(lat, lon, width, height, &bbox);
            if let Some(pixel_idx) = pixel_func(lat, lon) {
                let curr_val = results.entry(frame_no).or_insert(HashMap::new()).entry(pixel_idx).or_insert(0);
                if *curr_val < std::u16::MAX {
                    *curr_val += 1;
                }
            }

            num_nodes += 1;
            if num_nodes % 50_000_000 == 0 {
                println!("Read {} million points", num_nodes/1_000_000);
            }
        }
    }
    println!("Read {} points", num_nodes);
    let num_frames = last_frame_no - first_frame_no + 1;
    println!("There are {} frames, which is {} sec", num_frames, num_frames as f32/30.);

    let mut sorted_results = Vec::with_capacity((last_frame_no-first_frame_no+1) as usize);

    for frame_no in 0..num_frames {
        match results.remove(&(frame_no+first_frame_no)) {
            None => { sorted_results.push((frame_no, Vec::with_capacity(0))) },
            Some(pixels) => {
                let pixels = pixels.into_iter().collect();
                sorted_results.push((frame_no, pixels));
            }
        }
    }

    sorted_results
}

fn write_frames(frames: Frames, filename: &str, height: u32, width: u32, centre: &[f32; 2], sec_per_frame: u32, bbox: &[f32; 4], projection: &Projection) {
    let mut file = BufWriter::new(fs::File::create(&filename).unwrap());

    writeln!(file, "metadata version {}", env!("CARGO_PKG_VERSION")).expect("Couldn't write metadata");
    writeln!(file, "metadata height {}", height).expect("Couldn't write metadata");
    writeln!(file, "metadata width {}", width).expect("Couldn't write metadata");
    writeln!(file, "metadata sec_per_frame {}", sec_per_frame).expect("Couldn't write metadata");
    writeln!(file, "metadata bbox {},{},{},{}", bbox[0], bbox[1], bbox[2], bbox[3]).expect("Couldn't write metadata");
    writeln!(file, "metadata centre {},{}", centre[0], centre[1]).expect("Couldn't write metadata");
    match projection {
        &Projection::Ortho => {
            writeln!(file, "metadata projection ortho").expect("Couldn't write metadata");
        },
        &Projection::Equirect => {
            writeln!(file, "metadata projection equirect").expect("Couldn't write metadata");
        },
    }
    writeln!(file, "").expect("Couldn't write metadata");

    for (frame_no, pixels) in frames.into_iter() {
        write!(file, "{}", frame_no).unwrap();
        for p in pixels {
            write!(file, ",{},{}", p.0, p.1).unwrap();
        }
        write!(file, "\n").unwrap()
    }
}

fn read_metadata(filename: &str) -> Metadata {
    let file = BufReader::new(fs::File::open(&filename).unwrap());
    let metadata: HashMap<String, String> = file.lines().filter_map(|x| x.ok()).take_while(|x| x.len() > 0).map(|x| { let words: Vec<_> = x.split(" ").skip(1).take(2).collect(); (words[0].to_string(), words[1].to_string()) } ).collect();

    metadata
}

fn read_frames(filename: &str) -> Frames {
    let mut results = Frames::new();

    let file = BufReader::new(fs::File::open(&filename).unwrap());

    for line in file.lines().filter_map(|x| x.ok()).skip_while(|x| x.starts_with("metadata ") || x.len() == 0) {
        let frame_no = line.split(",").nth(0).unwrap().parse().unwrap();
        let pixels: Vec<(u32, u16)> = line.split(",").skip(1).collect::<Vec<_>>().chunks(2).map(|pair| (pair[0].parse().unwrap(), pair[1].parse().unwrap())).collect();
        results.push((frame_no, pixels))
    }

    results
}

fn latlon_to_pixel_index(lat: f32, lon: f32, width: u32, height: u32, bbox: &[f32; 4]) -> Option<u32> {
    let left = bbox[0]; let bottom = bbox[1]; let right = bbox[2]; let top = bbox[3];
    let bbox_width = right - left;
    let bbox_height = top - bottom;

    // FIXME should be able to do non-equals but it fails for point at south pole
    if lat >= top || lat <= bottom || lon >= right || lon <= left {
        return None;
    }

    let lat0 = top - lat;
    let lon0 = lon - left;


    let x = ((lon0/bbox_width)*(width as f32)) as u32;
    let y = ((lat0/bbox_height)*(height as f32)) as u32;

    let i = y * width + x;

    assert!(i < width*height, "{} L{}, lat = {} lon = {} width = {} height = {} bbox = {:?} x = {} y = {} i = {}", file!(), line!(), lat, lon, width, height, bbox, x, y, i);

    Some(i)
}

fn get_max_value(image: &Vec<Option<f32>>) -> f32 {
    let mut max = 0.;
    for pixel in image.iter().filter_map(|&x| x) {
        max = match pixel.partial_cmp(&max) {
            Some(Ordering::Greater) => pixel,
            _ => max,
        };
    }

    max
}

fn decay_image(image: &mut Vec<Option<f32>>) {
    for i in 0..image.len() {
        if image[i].is_some() && image[i].unwrap() > 0. {
            image[i] = image[i].map(|x| x*0.99);
        }
    }
}


fn create_gif(frames: Frames, output_image_filename: &str, height: u32, width: u32, colour_ramp: &ColourRamp) {
    let mut output_file = fs::File::create(output_image_filename).expect("Can't create image");

    // FIXME change width/height to u16?
    let mut encoder = gif::Encoder::new(&mut output_file, width as u16, height as u16, &colour_ramp.palette()).expect("Couldn't create encoder");
    encoder.set(gif::Repeat::Infinite).expect("Couldn't get inf repeat");

    let mut image = vec![None; (width*height) as usize];

    for (frame_no, pixels) in frames.into_iter() {

        decay_image(&mut image);

        for (i, magnitude) in pixels {
            // FIXME sometimes the value is invalid
            //assert!(i < width*height, "{} L{}, width = {} height = {} i = {}", file!(), line!(), width, height, i);
            if i < width*height {
                let new_value= image[i as usize].unwrap_or(0f32) + (magnitude as f32);
                image[i as usize] = Some(new_value);
            }
        }

        let max = get_max_value(&image);

        let mut pixels: Vec<u8> = Vec::with_capacity(image.len() * 4);
        for p in image.iter().cloned() {
            pixels.push(match p {
                None => 0,
                Some(x) => { (255f32 * (x/max)).round() as u8 },
            });
        }

        let mut frame = gif::Frame::from_indexed_pixels(width as u16, height as u16, pixels.as_mut_slice(), None);
        // 30 fps, and delay is in units of 10ms.
        frame.delay = 100 / 30;

        encoder.write_frame(&frame).expect("Couldn't write frame");

        if frame_no % 30 == 0 {
            println!("Wrote frame {}", frame_no);
        }

    }

}

fn create_frames(frames: Frames, output_image_filename: &str, height: u32, width: u32) {

    let mut image = vec![None; (width*height) as usize];

    for (frame_no, pixels) in frames.into_iter() {

        decay_image(&mut image);

        for (i, magnitude) in pixels {
            // FIXME sometimes the value is invalid
            //assert!(i < width*height, "{} L{}, width = {} height = {} i = {}", file!(), line!(), width, height, i);
            if i < width*height {
                let new_value= image[i as usize].unwrap_or(0f32) + (magnitude as f32);
                image[i as usize] = Some(new_value);
            }
        }

        let max = get_max_value(&image);

        let img = ImageBuffer::from_fn(width, height, |x, y| {
            let p = (y*width + x) as usize;
            let red = match image[p] { None => 0, Some(val) => { (255f32 * (val/max)).round() as u8 }, };
            image::Rgb([red, red, red])
        });


        let mut pixels: Vec<u8> = Vec::with_capacity(image.len() * 4);
        for p in image.iter().cloned() {
            pixels.push(match p {
                None => 0,
                Some(x) => { (255f32 * (x/max)).round() as u8 },
            });
        }


        let ref mut fout = fs::File::create(&format!("{}{:06}.png", output_image_filename, frame_no)).unwrap();
        image::ImageRgb8(img).save(fout, image::PNG).expect("Couldn't save image");


        if frame_no % 30 == 0 {
            println!("Wrote frame {}", frame_no);
        }

    }
    println!("Finished. You can convert this to a video with this command:\n\navconv -framerate 30  -i {}%06d.png output.mp4\n\n", output_image_filename);


}


enum Projection { Ortho, Equirect, }

fn main() {
    let matches = App::new("osm-history-animation")
                          .arg(Arg::with_name("input").long("input").short("i")
                               .takes_value(true))
                          .arg(Arg::with_name("output") .long("output").short("o")
                               .takes_value(true))
                          .arg(Arg::with_name("height") .long("height").short("h")
                               .takes_value(true))
                          .arg(Arg::with_name("spf") .long("sec-per-frame").short("s")
                               .takes_value(true))
                          .arg(Arg::with_name("colour_ramp") .long("colour-ramp")
                               .takes_value(true))
                          .arg(Arg::with_name("save-intermediate").long("save-intermediate"))
                          .arg(Arg::with_name("load-intermediate").long("load-intermediate"))
                          .arg(Arg::with_name("bbox").long("bbox").takes_value(true).short("b"))
                          // TODO add misspelling of "center"
                          .arg(Arg::with_name("centre").long("centre").takes_value(true).short("c"))
                          // TODO add conflicts, can't have --ortho and --equirect
                          .arg(Arg::with_name("ortho").long("ortho"))
                          .arg(Arg::with_name("equirect").long("equirect"))
                          .arg(Arg::with_name("gif").long("gif"))
                          .arg(Arg::with_name("frames").long("frames"))
                          .setting(AppSettings::AllowLeadingHyphen)
                          .get_matches();

    let input_filename = matches.value_of("input").unwrap();
    let output_filename = matches.value_of("output").unwrap();

    let metadata = if matches.is_present("load-intermediate") {
        read_metadata(input_filename)
    } else {
        HashMap::new()
    };

    let height: u32 = match matches.value_of("height") { None => metadata["height"].parse().unwrap(), Some(t) => t.parse().unwrap() };
    let sec_per_frame: u32 = match matches.value_of("spf") { None => metadata["sec_per_frame"].parse().unwrap(), Some(t) => t.parse().unwrap() };

    let bbox = match matches.value_of("bbox") {
        None => match metadata.get("bbox") {
            None => [-180., -90., 180., 90.],
            Some(text) => {
                let coords: Vec<f32> = text.split(",").map(|x| x.parse().unwrap() ).collect();
                [coords[0], coords[1], coords[2], coords[3]]
            }
        },
        Some(text) => {
            let coords: Vec<f32> = text.split(",").map(|x| x.parse().unwrap() ).collect();
            [coords[0], coords[1], coords[2], coords[3]]
        }
    };

    let centre = match matches.value_of("centre") {
        None => match metadata.get("centre") {
            None => [0., 0.],
            Some(c) => {
                let c: Vec<f32> = c.split(",").map(|x| x.parse().unwrap() ).collect();
                [c[0], c[1]]
            },
        },
        Some(text) => {
            let coords: Vec<f32> = text.split(",").map(|x| x.parse().unwrap() ).collect();
            [coords[0], coords[1]]
        }
    };

    let projection = if matches.is_present("ortho") {
        Projection::Ortho
    } else if matches.is_present("equirect") {
        Projection::Equirect
    } else {
        Projection::Equirect
    };

    let width = match projection {
        Projection::Ortho => { height },
        Projection::Equirect => {
            let left = bbox[0]; let bottom = bbox[1]; let right = bbox[2]; let top = bbox[3];
            let bbox_width = right - left;
            let bbox_height = top - bottom;
            ((bbox_width / bbox_height) * (height as f32)) as u32
        }
    };

    let pixel_func: Box<Fn(f32, f32) -> Option<u32>> = match projection {
        Projection::Ortho => {
            let ortho = orthoproj::OrthoProj::new(width, centre[0], centre[1], false);
            // FIXME think I have the y*width + x wrong....
            Box::new(move |lat, lon| { ortho.xy_for_pos(lat, lon).map(|(x, y)| y*width + x) })
        },
        Projection::Equirect => { Box::new(move |lat, lon| latlon_to_pixel_index(lat, lon, width, height, &bbox)) },
    };

    let frames = if matches.is_present("load-intermediate") {
        println!("Reading frames from {}", input_filename);
        read_frames(&input_filename)
    } else {
        println!("Reading PBF file {}", input_filename);
        read_pbf(&input_filename, sec_per_frame, pixel_func)
    };


    if matches.is_present("save-intermediate") {
        println!("Saving frame details to {}", output_filename);
        write_frames(frames, &output_filename, height, width, &centre, sec_per_frame, &bbox, &projection);
    } else {
        let colour_ramp = ColourRamp::new_from_filename(matches.value_of("colour_ramp").unwrap());
        if matches.is_present("frames") {
            println!("Creating frames with prefix {}", output_filename);
            create_frames(frames, &output_filename, height, width);
        } else {
            println!("Creating image {}", output_filename);
            create_gif(frames, &output_filename, height, width, &colour_ramp);
        }
    }
    println!("\nFinished");
}
