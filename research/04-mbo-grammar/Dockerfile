FROM pytorch/pytorch:2.5.1-cuda12.4-cudnn9-runtime

WORKDIR /experiment

RUN apt-get update && apt-get install -y --no-install-recommends gcc && rm -rf /var/lib/apt/lists/*
ENV CC=/usr/bin/gcc

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt && pip install --no-cache-dir awscli

COPY model.py data.py train.py train_finetune.py probe.py prepack_windows.py phase3_signal_check.py precompute_phase3.py ./
COPY run.sh run_phase2.sh run_preprocess.sh run_phase3.sh run_phase3_B.sh run_phase3_C.sh ./

RUN chmod +x run.sh run_phase2.sh run_preprocess.sh run_phase3.sh run_phase3_B.sh run_phase3_C.sh
