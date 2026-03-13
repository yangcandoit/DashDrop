/// <reference types="vite/client" />

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}

type BarcodeDetectorFormat = "qr_code";

interface DetectedBarcode {
  rawValue?: string;
}

interface BarcodeDetector {
  detect(source: ImageBitmapSource): Promise<DetectedBarcode[]>;
}

interface BarcodeDetectorConstructor {
  new (options?: { formats?: BarcodeDetectorFormat[] }): BarcodeDetector;
  getSupportedFormats?: () => Promise<BarcodeDetectorFormat[]>;
}

interface Window {
  BarcodeDetector?: BarcodeDetectorConstructor;
}
