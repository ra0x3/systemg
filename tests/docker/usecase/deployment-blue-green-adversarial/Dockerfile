FROM sysg-usecase-base

COPY tests/docker/usecase/deployment-blue-green-adversarial/server.py /usecase/server.py
COPY tests/docker/usecase/deployment-blue-green-adversarial/pre.sh /usecase/pre.sh
COPY tests/docker/usecase/deployment-blue-green-adversarial/switch.sh /usecase/switch.sh
COPY tests/docker/usecase/deployment-blue-green-adversarial/verify.sh /usecase/verify.sh
COPY tests/docker/usecase/deployment-blue-green-adversarial/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/deployment-blue-green-adversarial/run.sh /usecase/run.sh
RUN chmod +x /usecase/pre.sh /usecase/switch.sh /usecase/verify.sh /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
